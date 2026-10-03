//! Concrete persistence adapter for live capture ingress.
//!
//! The coordinator remains generic over its catalog and checkpoint ports.
//! This module is the deliberately narrow live-store factory: it owns the
//! database connection and keeps the concrete catalog implementation inside
//! an opaque coordinator reservation, so hook runtime code cannot call the
//! lifecycle mutation methods directly.
//!
//! It also owns the shared live hook boundary (ADR-ACF-10, ACF-17 subset):
//! the paired hook execution deadline, Libra's canonical session identity and
//! its telemetry redaction, bounded pure reads, the narrowed runtime envelope
//! and the repository hash-kind preflight used by both hook targets.
//!
//! ACF-19 adds the sanitized ingest span and the scoped, read-only
//! `agent_session` / `agent_checkpoint` probes used by the live pipeline and
//! checkpoint writers: the table-existence check, the first-writer owner
//! claim, the durable capture state, concurrent-session and identity-conflict
//! checks, and the latest committed checkpoint. Catalog mutations stay in the
//! catalog store.
//!
//! ACF-18 adds `effective_capture_deadline`, which turns a provider-owned
//! capture budget into the one paired runtime deadline without naming a
//! provider.
//!
//! ACF-20 adds the export-job runner lease: the `LiveExportLeasePort` store
//! port (production `ExportJobLeaseStore`, the only capture code that names
//! `export_job`), and the non-`Clone` `LiveExportRunner` token that runner
//! admission yields. The token's single terminal method, `settle`, applies
//! one `LeaseDisposition` with its site's fixed `reason=` and tracing fields;
//! the port's release and advance methods are called nowhere else. Coverage
//! claims are not part of this lease.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Utc;
use git_internal::hash::set_hash_kind;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement};
use serde_json::Map;
use thiserror::Error;

use crate::internal::{
    ai::{
        capture::{
            catalog::{
                CaptureCatalogApplyRequest, CaptureCatalogApplyResult, CaptureCatalogDiagnostic,
                CaptureCatalogPort, CaptureCatalogRetryableStage, CaptureCatalogStore,
            },
            coordinator::{
                CaptureCoordinatorError, CaptureCoordinatorReservation,
                CaptureCoordinatorReserveOutcome,
                reserve_capture_catalog as reserve_catalog_through_port,
            },
            ingress::{CaptureDeadline, CaptureEventContext, CaptureRuntimeScope},
            scope_binding::{ActiveRepositoryFailureClass, active_repository_failure},
            state::{CapturePhase, DurableCaptureState},
        },
        capture_scope::{CaptureCommitDeadline, CaptureScope},
        hooks::{
            lifecycle::{LifecycleEventKind, SessionHookEnvelope},
            provider::{HookProvider, ProviderHookCommand},
        },
    },
    config::ConfigKv,
};

/// Concrete live-hook reservation. The catalog port remains opaque to hook
/// runtime; it can only be consumed through `execute_preapplied`.
pub(crate) type LiveCaptureReservation = CaptureCoordinatorReservation<CaptureCatalogStore>;

/// Start a live reservation under one invocation's transient absolute
/// deadline. The deadline is retained by the concrete catalog port carried in
/// the opaque reservation; it is not persisted in the request or receipt.
pub(crate) async fn reserve_capture_catalog_until(
    conn: DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<CaptureCoordinatorReserveOutcome<CaptureCatalogStore>, CaptureCoordinatorError> {
    let catalog = match deadline {
        Some(deadline) => CaptureCatalogStore::new_until(conn, deadline),
        None => CaptureCatalogStore::new(conn),
    };
    let result = reserve_catalog_through_port(&catalog, request).await?;
    match result {
        // The apply result is deterministic for the just-created store;
        // repeat neither reducer nor database work.
        result @ (CaptureCatalogApplyResult::Applied { .. }
        | CaptureCatalogApplyResult::ResumePending { .. }) => {
            Ok(CaptureCoordinatorReserveOutcome::Reserved(Box::new(
                CaptureCoordinatorReservation::from_preapplied(catalog, request.clone(), result),
            )))
        }
        CaptureCatalogApplyResult::AlreadyApplied => {
            Ok(CaptureCoordinatorReserveOutcome::AlreadyApplied)
        }
        CaptureCatalogApplyResult::ConflictUnchanged { conflict } => {
            Ok(CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict })
        }
    }
}

/// Record a live diagnostic only while the ordinary capture budget remains
/// live. A caller may still release an already-reserved provisional resource
/// after this returns; it must not create a new diagnostic after expiry.
pub(crate) async fn record_live_maintenance_lock_failure_until(
    conn: DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    deadline: Option<CaptureCommitDeadline>,
) {
    update_live_retryable_diagnostic(
        conn,
        request,
        CaptureCatalogDiagnostic::RecordRetryableCheckpointFailure {
            stage: CaptureCatalogRetryableStage::MaintenanceLock,
            failed_at: Utc::now().timestamp(),
        },
        deadline,
    )
    .await;
}

/// Deadline-bounded variant used by ordinary hook execution. See
/// [`record_live_maintenance_lock_failure_until`] for why cleanup remains
/// separate from this mutation.
pub(crate) async fn record_live_checkpoint_write_failure_until(
    conn: DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    deadline: Option<CaptureCommitDeadline>,
) {
    update_live_retryable_diagnostic(
        conn,
        request,
        CaptureCatalogDiagnostic::RecordRetryableCheckpointFailure {
            stage: CaptureCatalogRetryableStage::CheckpointWrite,
            failed_at: Utc::now().timestamp(),
        },
        deadline,
    )
    .await;
}

/// Deadline-bounded variant used by ordinary hook execution.
pub(crate) async fn clear_live_retryable_checkpoint_failure_until(
    conn: DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    deadline: Option<CaptureCommitDeadline>,
) {
    update_live_retryable_diagnostic(
        conn,
        request,
        CaptureCatalogDiagnostic::ClearRetryableCheckpointFailure,
        deadline,
    )
    .await;
}

async fn update_live_retryable_diagnostic(
    conn: DatabaseConnection,
    request: &CaptureCatalogApplyRequest,
    diagnostic: CaptureCatalogDiagnostic,
    deadline: Option<CaptureCommitDeadline>,
) {
    let catalog = match deadline {
        Some(deadline) => CaptureCatalogStore::new_until(conn, deadline),
        None => CaptureCatalogStore::new(conn),
    };
    match catalog.update_diagnostic(request, diagnostic).await {
        Ok(true) => {
            tracing::debug!(
                target: "agent.capture.diagnostic",
                replay_key = %request.action().action_key(),
                diagnostic_outcome = "updated",
                "capture retry diagnostic updated"
            );
        }
        Ok(false) => {
            tracing::debug!(
                target: "agent.capture.diagnostic",
                replay_key = %request.action().action_key(),
                diagnostic_outcome = "fenced",
                "capture retry diagnostic was not applied after a concurrent mutation"
            );
        }
        Err(_) => {
            // Catalog errors may embed backend detail. Keep hook-side
            // observability content-free; a later replay can repair it.
            tracing::warn!(
                target: "agent.capture.diagnostic",
                replay_key = %request.action().action_key(),
                diagnostic_outcome = "unavailable",
                "capture retry diagnostic could not be persisted"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Shared live hook boundary (ADR-ACF-10, ACF-17 subset): the paired execution
// deadline, canonical session identity and its telemetry redaction, bounded
// pure reads, the narrowed runtime envelope and the repository hash-kind
// preflight shared by the legacy intent writer and AgentTraces capture.
// ---------------------------------------------------------------------------

/// Separator inserted between provider name and the provider's native session ID
/// when forming Libra's namespaced AI session ID.
const SESSION_ID_DELIMITER: &str = "__";

/// A managed hook must never inherit the ordinary 30-second SQLite busy
/// timeout. A short per-statement slice lets an uncongested terminal receipt
/// persist while returning a lock/contention failure to the provider before
/// its host deadline is consumed.
pub(crate) const HOOK_DATABASE_BUSY_TIMEOUT_CAP: Duration = Duration::from_millis(200);
/// Managed provider configurations reserve at least 500ms beyond capture's
/// own deadline. A terminal callback may use this small fixed slice only to
/// settle its content-free receipt after the main capture window elapses;
/// it is never a new checkpoint/snapshot budget.
pub(crate) const HOOK_TERMINAL_SETTLEMENT_GRACE: Duration = Duration::from_millis(250);

/// A capture deadline represented in the two clock domains needed by a
/// killable helper. The monotonic value is the parent-side liveness authority;
/// the wall-clock value is only a second mutation gate inside the child.
#[derive(Clone, Copy)]
pub(crate) struct HookExecutionDeadline {
    pub(crate) monotonic: Instant,
    pub(crate) absolute_millis: i64,
}

impl HookExecutionDeadline {
    /// Preserve the two deadline clocks established at hook dispatch for the
    /// final database authorization boundary.  Runtime code must never derive
    /// a fresh wall deadline from a remaining monotonic duration.
    pub(crate) fn commit_deadline(self) -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_established_pair(self.monotonic, self.absolute_millis)
    }
}

/// Cross the generic ingress-time boundary into the durable catalog/checkpoint
/// authorization pair. Keeping this conversion in the runtime prevents the
/// untrusted ingress parser from acquiring a persistence dependency while
/// preserving the two clocks sampled at dispatch.
pub(crate) fn capture_commit_deadline(deadline: CaptureDeadline) -> CaptureCommitDeadline {
    CaptureCommitDeadline::from_established_pair(deadline.monotonic(), deadline.absolute_millis())
}

/// A pure managed-capture database read used all of its established runtime
/// budget.  Keeping this distinct from a SQLite error lets a terminal abandon
/// normal work and enter its already-reserved content-free settlement path.
#[derive(Debug, Error)]
#[error("managed AgentTraces pure read exceeded its execution deadline")]
struct AgentTracesReadDeadlineExceeded;

fn agent_traces_read_deadline_error(operation: &'static str) -> anyhow::Error {
    anyhow::Error::new(AgentTracesReadDeadlineExceeded).context(operation)
}

pub(crate) fn is_agent_traces_read_deadline(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<AgentTracesReadDeadlineExceeded>())
}

/// Await a connection-level read only until its already-established deadline.
///
/// This helper is intentionally restricted to pure reads: do not use it for
/// catalog DML, final commit authorization, rollback, or COMMIT
/// acknowledgement.  Cancelling any of those operations after SQLite has
/// dispatched it could report a timeout while leaving a durable mutation.
pub(crate) async fn await_agent_traces_pure_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let Some(deadline) = deadline else {
        return read.await.context(operation);
    };
    if Instant::now() >= deadline.monotonic() {
        return Err(agent_traces_read_deadline_error(operation));
    }
    let result =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), read)
            .await
        {
            Ok(result) => result,
            Err(_) => return Err(agent_traces_read_deadline_error(operation)),
        };
    // `timeout_at` may observe a ready future in the same poll in which the
    // clock advances.  A completed read is safe to discard, but it must not
    // let the caller begin normal capture work after the primary deadline.
    if Instant::now() >= deadline.monotonic() {
        return Err(agent_traces_read_deadline_error(operation));
    }
    result.context(operation)
}

/// Combine a provider name with the provider's native session ID into Libra's
/// canonical ID.
///
/// Functional scope: the resulting string is used as a directory name and as a
/// metadata key, so it must round-trip without escaping. Both inputs are assumed
/// to come from validated [`CaptureIngressCommand`](crate::internal::ai::capture::ingress::CaptureIngressCommand) values. This is a durable
/// identity, not a telemetry value; log it only through [`redact_session_id`].
pub fn build_ai_session_id(provider: &str, provider_session_id: &str) -> String {
    format!("{provider}{SESSION_ID_DELIMITER}{provider_session_id}")
}

/// Return the fixed session-identity token allowed in logs and diagnostics.
///
/// Provider prefixes are part of the untrusted session spelling: for example,
/// a canonical `codex__…` ID can place native secret material inside its first
/// eight characters. Keep no substring, hash, or other offline-enumerable
/// derivative in telemetry.
pub(crate) fn redact_session_id(_session_id: &str) -> &'static str {
    "***"
}

/// Establish the effective AgentTraces capture deadline (ADR-ACF-10, ACF-18).
///
/// A host-provided managed pair is always kept unchanged. When the host
/// supplied none, a provider whose live capability owns a bounded budget (an
/// export subprocess deadline) establishes one paired runtime deadline from
/// it, before any catalog, snapshot, coverage, checkpoint or
/// terminal-settlement work, so a successful source cannot leave unbounded
/// durable work behind it. Providers without such a budget keep the legacy
/// no-deadline behavior. The hook entry calls this before ingress scope
/// binding and the live pipeline calls it again; with a pair already
/// established the second call is the identity.
pub(crate) fn effective_capture_deadline(
    missing_host_budget: Option<Duration>,
    deadline: Option<CaptureDeadline>,
) -> Result<Option<CaptureDeadline>> {
    if deadline.is_some() {
        return Ok(deadline);
    }
    let Some(budget) = missing_host_budget else {
        return Ok(deadline);
    };
    let budget_millis = u64::try_from(budget.as_millis())
        .context("provider-owned capture budget exceeds the hook budget range")?;
    CaptureDeadline::from_budget_millis(budget_millis)
        .map(Some)
        .context("establish provider-owned capture deadline")
}

pub(crate) fn hook_execution_deadline(
    deadline: CaptureDeadline,
    terminal: bool,
) -> Result<HookExecutionDeadline> {
    let grace = if terminal {
        HOOK_TERMINAL_SETTLEMENT_GRACE
    } else {
        Duration::ZERO
    };
    let monotonic = deadline.monotonic().checked_add(grace).context(
        "capture terminal settlement deadline exceeds this platform's monotonic clock range",
    )?;
    let grace_millis = i64::try_from(grace.as_millis())
        .context("capture terminal settlement deadline exceeds the persistent range")?;
    let absolute_millis = deadline
        .absolute_millis()
        .checked_add(grace_millis)
        .context("capture terminal settlement deadline exceeds the persistent range")?;
    Ok(HookExecutionDeadline {
        monotonic,
        absolute_millis,
    })
}

/// Reconstruct the deliberately narrow runtime context expected by legacy
/// adapters from ingress-owned facts. The original `SessionHookEnvelope` is
/// dropped before this point, so no flattened provider fields or raw event
/// spelling can accidentally flow into metadata, logs, or a provider hook.
pub(crate) fn narrow_envelope_from_capture_context(
    context: CaptureEventContext,
    event_kind: LifecycleEventKind,
) -> (SessionHookEnvelope, CaptureRuntimeScope) {
    let CaptureEventContext {
        provider_session_id,
        working_dir,
        runtime_scope,
    } = context;
    (
        SessionHookEnvelope {
            hook_event_name: event_kind.to_string(),
            session_id: provider_session_id,
            cwd: working_dir,
            // Raw provider pointers are not part of the generic capture
            // handoff. A later snapshot resolver may derive a source from the
            // verified cwd and provider session id, or fall back safely.
            transcript_path: None,
            extra: Map::new(),
        },
        runtime_scope,
    )
}

///
/// Mirrors `cli::set_local_hash_kind_for_storage` but reads via an explicitly-open
/// connection that the hook runtime obtains. Defaults to `sha1` for repositories
/// initialised before SHA-256 support landed.
///
/// Config **read errors** propagate (fail-closed). A missing key (`Ok(None)`)
/// keeps the sha1 default so pre-objectformat repositories stay usable.
/// Failures are classified by [`read_repository_hash_kind`].
pub(crate) async fn set_hash_kind_from_connection(conn: &DatabaseConnection) -> Result<()> {
    let hash_kind = read_repository_hash_kind(conn).await?;
    set_hash_kind(hash_kind);
    Ok(())
}

/// Read and validate the repository's `core.objectformat` without changing
/// the process-wide hash kind.
///
/// A failure carries only the closed
/// [`ActiveRepositoryFailureClass::ObjectFormatUnreadable`] /
/// [`ActiveRepositoryFailureClass::ObjectFormatUnsupported`] class (never the
/// stored value or a database message), so fail-closed hook surfaces can
/// restore the preflight's `LBR-IO-001` / `LBR-REPO-002` contract.
pub(crate) async fn read_repository_hash_kind(
    conn: &DatabaseConnection,
) -> Result<git_internal::hash::HashKind> {
    let lookup = ConfigKv::get_with_conn(conn, "core.objectformat")
        .await
        .map(|entry| entry.map(|e| e.value));
    let class = if lookup.is_err() {
        ActiveRepositoryFailureClass::ObjectFormatUnreadable
    } else {
        ActiveRepositoryFailureClass::ObjectFormatUnsupported
    };
    hash_kind_from_object_format_lookup(lookup).map_err(|_| active_repository_failure(class))
}

/// Map a `core.objectformat` lookup onto [`HashKind`].
///
/// - `Ok(Some(value))` → [`crate::internal::object_format::parse_config_value`]
/// - `Ok(None)` → `HashKind::Sha1` (legacy repos without the key)
/// - `Err(_)` → propagated (fail-closed; never swallowed into sha1)
pub(crate) fn hash_kind_from_object_format_lookup(
    lookup: Result<Option<String>>,
) -> Result<git_internal::hash::HashKind> {
    let raw = match lookup {
        Ok(Some(value)) => value,
        Ok(None) => "sha1".to_string(),
        Err(error) => {
            return Err(error).context("failed to read core.objectformat from repository config");
        }
    };
    crate::internal::object_format::parse_config_value(&raw)
}

/// Record a rejected test frame without allowing the frame to enter runtime
/// state. The raw test frame is lowered in `capture::ingress`; production has
/// no byte-oriented runtime entrypoint.
pub(crate) fn record_in_process_ingress_validation_failure(
    command: ProviderHookCommand,
    provider: &dyn HookProvider,
) {
    let ingest_span = new_ingest_span(command, provider);
    ingest_span.record("validated", false);
}

/// Open the sanitized hook-ingest span before parsing. Raw frame bytes and
/// payload fields are never attached to it.
pub(crate) fn new_ingest_span(
    command: ProviderHookCommand,
    provider: &dyn HookProvider,
) -> tracing::Span {
    tracing::info_span!(
        "agent.hook.ingest",
        provider = provider.provider_name(),
        verb = %command,
        event_kind = tracing::field::Empty,
        event_id = tracing::field::Empty,
        provider_source = tracing::field::Empty,
        frame_bytes = tracing::field::Empty,
        validated = tracing::field::Empty,
        partial = tracing::field::Empty,
    )
}

/// Thin runtime adapter from the existing catalog row to the pure reducer's
/// durable input. A later catalog store owns this query; keeping it narrow
/// here prevents the reducer from acquiring a database dependency.
pub(crate) async fn load_capture_state(
    conn: &sea_orm::DatabaseConnection,
    agent_kind: &str,
    provider_session_id: &str,
    scope: &CaptureScope,
) -> Result<Option<DurableCaptureState>> {
    use sea_orm::{ConnectionTrait, Statement};

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT state, stopped_at, sync_revision
             FROM agent_session
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
             LIMIT 1",
            [
                agent_kind.into(),
                provider_session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("read durable agent capture state")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let state: String = row
        .try_get("", "state")
        .context("decode durable agent capture state")?;
    let phase = CapturePhase::from_db(&state).context("validate durable agent capture state")?;
    let stopped_at: Option<i64> = row
        .try_get("", "stopped_at")
        .context("decode durable agent capture stopped_at")?;
    let sync_revision: i64 = row
        .try_get("", "sync_revision")
        .context("decode durable agent capture sync revision")?;
    Ok(Some(DurableCaptureState {
        phase,
        stopped_at,
        sync_revision,
    }))
}

/// Probe for the `agent_session` catalog table. The migration may not have
/// run in an older repository; the caller fails loud when this returns `None`.
pub(crate) async fn agent_session_table_exists(
    conn: &DatabaseConnection,
    backend: DatabaseBackend,
) -> Result<Option<QueryResult>> {
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
        [],
    ))
    .await
    .context("query sqlite_master")
}

/// Read the first-writer owner claim for a provider session inside the
/// trusted capture scope: the row's `agent_kind` column, ordered by `rowid`
/// so the earliest physical claim wins. `context` keeps each caller's own
/// error context.
pub(crate) async fn scoped_owner_agent_kind(
    conn: &DatabaseConnection,
    backend: DatabaseBackend,
    envelope: &SessionHookEnvelope,
    scope: &CaptureScope,
    context: &'static str,
) -> Result<Option<QueryResult>> {
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT agent_kind FROM agent_session WHERE provider_session_id = ? \
         AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ? \
         AND workspace_id IS ? AND workspace_fence IS ? \
         ORDER BY rowid ASC LIMIT 1",
        [
            envelope.session_id.clone().into(),
            scope.repo_id.clone().into(),
            scope.worktree_id.clone().into(),
            scope.workspace_id.clone().into(),
            scope.workspace_fence.into(),
        ],
    ))
    .await
    .context(context)
}

/// Distinguish a same-provider identity change (this provider session is
/// recorded under another verified working directory or local session id)
/// from the cross-provider owner race, which the catalog reports with the
/// same content-free `SessionIdentity` conflict. Reads only; the compared
/// identities never leave this query.
pub(crate) async fn terminal_session_identity_changed(
    conn: &sea_orm::DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
    agent_kind: &str,
    envelope: &SessionHookEnvelope,
    session_id: &str,
) -> Result<bool> {
    use sea_orm::{ConnectionTrait, Statement};

    await_agent_traces_pure_read_until(
        deadline,
        "classify terminal capture identity conflict",
        async {
            Ok(conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 FROM agent_session WHERE agent_kind = ? AND provider_session_id = ? \
                     AND (session_id <> ? OR working_dir <> ?) LIMIT 1",
                    [
                        agent_kind.into(),
                        envelope.session_id.clone().into(),
                        session_id.into(),
                        envelope.cwd.clone().into(),
                    ],
                ))
                .await
                .context("query recorded terminal capture identity")?
                .is_some())
        },
    )
    .await
}

/// Detect whether a `TurnStart` is starting alongside another active agent
/// session in the same `working_dir`.
///
/// Per the traces state machine (`docs/development/commands/_general.md` §6.3),
/// a `TurnStart` (UserPromptSubmit) checks for other `active` sessions in
/// the same `working_dir`; finding any records `concurrent_active=true`
/// without blocking. Only `TurnStart` events newly raise the flag; the
/// marker's stickiness across the rest of the session is handled by the
/// metadata upsert, which merges (`json_patch`) rather than overwrites, so a
/// once-set `concurrent_active=true` survives later events that omit it.
pub(crate) async fn session_concurrent_active(
    conn: &sea_orm::DatabaseConnection,
    backend: sea_orm::DatabaseBackend,
    event_kind: LifecycleEventKind,
    libra_session_id: &str,
    working_dir: &str,
    scope: &CaptureScope,
) -> Result<bool> {
    use sea_orm::{ConnectionTrait, Statement};

    // Only a fresh turn newly detects concurrency.
    if !matches!(event_kind, LifecycleEventKind::TurnStart) {
        return Ok(false);
    }

    // Count *other* active sessions sharing this working_dir.
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS peers FROM agent_session \
             WHERE state = 'active' AND working_dir = ? AND session_id <> ? \
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ? \
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                working_dir.into(),
                libra_session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("failed to count concurrent active sessions")?;
    let peers = row
        .and_then(|row| row.try_get_by::<i64, _>("peers").ok())
        .unwrap_or(0);
    Ok(peers > 0)
}

/// Most-recent `committed` checkpoint id for a session, used as the
/// `parent_checkpoint_id` linkage of a freshly-materialised subagent
/// checkpoint. Returns `None` when the session has no committed checkpoint
/// yet (the subagent ran before the first turn/session checkpoint landed).
pub(crate) async fn latest_committed_checkpoint_id(
    conn: &sea_orm::DatabaseConnection,
    session_id: &str,
) -> Result<Option<String>> {
    use sea_orm::{ConnectionTrait, Statement};

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint \
             WHERE session_id = ? AND scope = 'committed' \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [session_id.into()],
        ))
        .await
        .context("failed to resolve latest committed checkpoint for subagent linkage")?;
    row.map(|r| r.try_get::<String>("", "checkpoint_id"))
        .transpose()
        .context("failed to read parent committed checkpoint id")
}

// ---------------------------------------------------------------------------
// Export-job runner lease (ADR-ACF-10, ACF-20). A provider's transcript
// exporter never receives this lease; capture holds it around the export and
// settles it exactly once per admitted runner.
// ---------------------------------------------------------------------------

/// The per-session export job a runner lease belongs to.
#[derive(Clone, Copy)]
pub(crate) struct LiveExportTarget<'a> {
    pub(crate) agent_kind: &'a str,
    pub(crate) provider_session_id: &'a str,
    pub(crate) scope: &'a CaptureScope,
}

/// What observing one idle granted this invocation (ADR-DR-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveExportAdmission {
    /// Another runner holds an unexpired lease; this idle was recorded for
    /// it (or a later idle) to cover.
    RecordedOnly,
    /// This invocation owns the runner lease up to `target_generation`.
    Runner {
        fence_token: i64,
        target_generation: i64,
    },
}

/// How a settled runner lease leaves its export job. A release never
/// publishes `idle` and always uses the narrow recovery grace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseDisposition {
    /// The runner cannot yield a trusted transcript: `failed` with
    /// `LBR-AGENT-005`, actionable for a later idle or doctor.
    Failed,
    /// Retire only the lease as `dirty` so a later idle retries.
    Dirty,
    /// Advance the processed generation and release honestly under the
    /// primary deadline. A failed advance retires the lease as `dirty` and is
    /// returned to the caller.
    AdvanceAndRelease,
    /// Leave the lease to expire; no database write.
    LeaveToExpiry,
}

/// Tracing fields of a settlement site's warning: the checkpoint stage names
/// its checkpoint, every other site the redacted session identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseLogContext<'a> {
    Checkpoint {
        checkpoint_id: &'a str,
        message: &'static str,
    },
    Session {
        session_id: &'a str,
        message: &'static str,
    },
}

impl LeaseLogContext<'_> {
    fn warn(self, reason: &'static str) {
        match self {
            Self::Checkpoint {
                checkpoint_id,
                message,
            } => tracing::warn!(checkpoint_id = %checkpoint_id, reason, "{message}"),
            Self::Session {
                session_id,
                message,
            } => tracing::warn!(
                reason,
                session_id = %redact_session_id(session_id),
                "{message}"
            ),
        }
    }
}

/// Store side of the export-job runner lease. It has no coverage-claim
/// method: claim cleanup stays with the checkpoint writer. The release and
/// advance methods are called only by [`LiveExportRunner::settle`].
pub(crate) trait LiveExportLeasePort: Sync {
    /// Observe one idle and try to become the job's runner.
    fn observe_idle(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        now_ms: i64,
        deadline: CaptureCommitDeadline,
    ) -> impl Future<Output = Result<LiveExportAdmission>> + Send;

    /// Retire the runner as `failed` with `LBR-AGENT-005` (recovery grace).
    fn release_failed(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        deadline: CaptureCommitDeadline,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Retire only the runner lease as `dirty` (recovery grace).
    fn release_dirty(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        deadline: CaptureCommitDeadline,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Advance the processed generation and release under the primary
    /// deadline (ADR-DR-11).
    fn advance_and_release(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        target_generation: i64,
        deadline: CaptureCommitDeadline,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// The production export-job lease store: the only capture code that names
/// `export_job`. Lease SQL, row format and round trips are unchanged.
pub(crate) struct ExportJobLeaseStore<'c> {
    conn: &'c DatabaseConnection,
}

impl<'c> ExportJobLeaseStore<'c> {
    pub(crate) fn new(conn: &'c DatabaseConnection) -> Self {
        Self { conn }
    }
}

impl LiveExportLeasePort for ExportJobLeaseStore<'_> {
    async fn observe_idle(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        now_ms: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<LiveExportAdmission> {
        match crate::internal::ai::export_job::observe_idle_until(
            self.conn,
            target.agent_kind,
            target.provider_session_id,
            target.scope,
            owner,
            now_ms,
            Some(deadline),
        )
        .await?
        {
            crate::internal::ai::export_job::IdleOutcome::RecordedOnly => {
                Ok(LiveExportAdmission::RecordedOnly)
            }
            crate::internal::ai::export_job::IdleOutcome::Runner {
                fence_token,
                target_generation,
                ..
            } => Ok(LiveExportAdmission::Runner {
                fence_token,
                target_generation,
            }),
        }
    }

    async fn release_failed(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<()> {
        crate::internal::ai::export_job::release_with_recovery_grace(
            self.conn,
            &crate::internal::ai::export_job::ExportJobTarget::new(
                target.agent_kind,
                target.provider_session_id,
                target.scope,
            ),
            crate::internal::ai::export_job::ExportLeaseRelease::new(
                owner,
                fence_token,
                "failed",
                Some("LBR-AGENT-005"),
                Utc::now().timestamp_millis(),
            ),
            deadline,
        )
        .await
    }

    async fn release_dirty(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<()> {
        crate::internal::ai::export_job::release_with_recovery_grace(
            self.conn,
            &crate::internal::ai::export_job::ExportJobTarget::new(
                target.agent_kind,
                target.provider_session_id,
                target.scope,
            ),
            crate::internal::ai::export_job::ExportLeaseRelease::new(
                owner,
                fence_token,
                "dirty",
                None,
                Utc::now().timestamp_millis(),
            ),
            deadline,
        )
        .await
    }

    async fn advance_and_release(
        &self,
        target: &LiveExportTarget<'_>,
        owner: &str,
        fence_token: i64,
        target_generation: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<()> {
        crate::internal::ai::export_job::advance_and_release_until(
            self.conn,
            &crate::internal::ai::export_job::ExportJobTarget::new(
                target.agent_kind,
                target.provider_session_id,
                target.scope,
            ),
            owner,
            fence_token,
            target_generation,
            Utc::now().timestamp_millis(),
            deadline,
        )
        .await
        .map(|_| ())
    }
}

/// The export-job runner lease this invocation was admitted to. It is
/// neither `Clone` nor `Copy`, and its only terminal method is
/// [`Self::settle`]. Debug builds panic when a token is dropped unsettled
/// outside an unwind, so a new exit path cannot silently strand an
/// `inflight` lease.
pub(crate) struct LiveExportRunner<'a, P: LiveExportLeasePort> {
    port: &'a P,
    target: LiveExportTarget<'a>,
    owner: String,
    now_ms: i64,
    fence_token: i64,
    target_generation: i64,
    deadline: CaptureCommitDeadline,
    #[cfg(debug_assertions)]
    settled: bool,
}

impl<'a, P: LiveExportLeasePort> LiveExportRunner<'a, P> {
    /// Observe one idle under `deadline`. `None` means another runner holds
    /// the lease and was recorded to cover this idle.
    pub(crate) async fn admit(
        port: &'a P,
        target: LiveExportTarget<'a>,
        deadline: CaptureCommitDeadline,
    ) -> Result<Option<Self>> {
        let owner = format!("export:{}:{}", std::process::id(), uuid::Uuid::new_v4());
        let now_ms = Utc::now().timestamp_millis();
        match port.observe_idle(&target, &owner, now_ms, deadline).await? {
            LiveExportAdmission::RecordedOnly => Ok(None),
            LiveExportAdmission::Runner {
                fence_token,
                target_generation,
            } => Ok(Some(Self {
                port,
                target,
                owner,
                now_ms,
                fence_token,
                target_generation,
                deadline,
                #[cfg(debug_assertions)]
                settled: false,
            })),
        }
    }

    /// The opaque `export:{pid}:{uuid}` owner that also owns this runner's
    /// export-channel coverage claims.
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// The admission timestamp used for this runner's coverage claims.
    pub(crate) fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Apply `disposition` to the lease exactly once. Only a failed
    /// `AdvanceAndRelease` advance returns `Err` (after retiring the lease as
    /// `dirty`); a failed release is logged with `reason` and `log`.
    pub(crate) async fn settle(
        mut self,
        disposition: LeaseDisposition,
        reason: &'static str,
        log: LeaseLogContext<'_>,
    ) -> Result<()> {
        self.mark_settled();
        let released = match disposition {
            LeaseDisposition::Failed => {
                self.port
                    .release_failed(&self.target, &self.owner, self.fence_token, self.deadline)
                    .await
            }
            LeaseDisposition::Dirty => {
                self.port
                    .release_dirty(&self.target, &self.owner, self.fence_token, self.deadline)
                    .await
            }
            LeaseDisposition::AdvanceAndRelease => {
                let Err(error) = self
                    .port
                    .advance_and_release(
                        &self.target,
                        &self.owner,
                        self.fence_token,
                        self.target_generation,
                        self.deadline,
                    )
                    .await
                else {
                    return Ok(());
                };
                // A normal completion may never borrow recovery grace to
                // publish `idle`: retire only this runner as `dirty`, then
                // let the caller fail its continuation for a retry.
                if self
                    .port
                    .release_dirty(&self.target, &self.owner, self.fence_token, self.deadline)
                    .await
                    .is_err()
                {
                    log.warn(reason);
                }
                return Err(error);
            }
            LeaseDisposition::LeaveToExpiry => return Ok(()),
        };
        if released.is_err() {
            log.warn(reason);
        }
        Ok(())
    }

    fn mark_settled(&mut self) {
        #[cfg(debug_assertions)]
        {
            self.settled = true;
        }
    }
}

#[cfg(debug_assertions)]
impl<P: LiveExportLeasePort> Drop for LiveExportRunner<'_, P> {
    fn drop(&mut self) {
        if !self.settled && !std::thread::panicking() {
            panic!("an export runner lease was dropped without settlement");
        }
    }
}
