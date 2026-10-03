//! Provider-neutral live capture pipeline for validated AgentTraces hooks
//! (ADR-ACF-10, ACF-19).
//!
//! `hooks::runtime` dispatches a validated ingress command here. This module
//! opens the bounded hook database, resolves the trusted capture scope,
//! settles an expired terminal callback through the content-free pending
//! path, redacts the lifecycle event, and drives the catalog reservation and
//! coordinator handoff. Checkpoint writers and their reservation settlement
//! live in `capture::live_checkpoint`; the scoped read probes and the ingest
//! span live in `capture::live`. Every provider-specific decision comes from
//! the callback's `LiveCaptureBinding` (ADR-ACF-10, ACF-18): this module
//! never compares a provider name or kind. The export-job lease port is
//! injected here (production: `ExportJobLeaseStore` over the hook connection)
//! and handed to the committed checkpoint writer (ACF-20).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde_json::json;

use crate::{
    internal::{
        ai::{
            capture::{
                catalog::{
                    CaptureCatalogAction, CaptureCatalogApplyRequest, CaptureCatalogConflict,
                    CaptureCatalogError, CaptureCatalogMetadataPatch, CaptureCatalogMutation,
                    CaptureCatalogRedactionReport, CaptureCatalogSession,
                },
                checkpoint::checkpoint_id_for_capture_action,
                coordinator::{
                    CaptureCoordinator, CaptureCoordinatorError, CaptureCoordinatorFinalizer,
                    CaptureCoordinatorOutcome, CaptureCoordinatorRequest,
                    CaptureCoordinatorReserveOutcome, NoCheckpointStore,
                },
                finalizer::{CaptureFinalizeMode, CaptureFinalizePolicy},
                ingress::{CaptureDeadline, CaptureIngressCommand, CaptureIngressOutcome},
                live::{
                    ExportJobLeaseStore, HOOK_DATABASE_BUSY_TIMEOUT_CAP,
                    HOOK_TERMINAL_SETTLEMENT_GRACE, HookExecutionDeadline, LiveExportLeasePort,
                    agent_session_table_exists, await_agent_traces_pure_read_until,
                    build_ai_session_id, capture_commit_deadline,
                    clear_live_retryable_checkpoint_failure_until, effective_capture_deadline,
                    hook_execution_deadline, is_agent_traces_read_deadline, load_capture_state,
                    narrow_envelope_from_capture_context, new_ingest_span,
                    record_live_checkpoint_write_failure_until,
                    record_live_maintenance_lock_failure_until, redact_session_id,
                    reserve_capture_catalog_until, scoped_owner_agent_kind,
                    session_concurrent_active, set_hash_kind_from_connection,
                    terminal_session_identity_changed,
                },
                live_checkpoint::{
                    append_redaction_report_bounded, finalizer_now_millis,
                    settle_preapplied_without_checkpoint, write_committed_checkpoint,
                    write_subagent_checkpoint,
                },
                scope_binding::{ActiveRepositoryFailureClass, active_repository_failure},
                state::{CapturePhase, CheckpointWrite, LifecycleReducerInput, reduce_lifecycle},
            },
            capture_scope::{CaptureCommitDeadline, CaptureScope},
            hooks::{
                lifecycle::{LifecycleEventKind, SessionHookEnvelope},
                provider::{HookProvider, ProviderHookCommand},
            },
            observed_agents::live_capture::{LiveCaptureBinding, LiveCaptureContext},
            subagent_content::SubagentDiscovery,
        },
        db,
    },
    utils::util,
};

#[cfg(test)]
pub(super) mod test_support {
    use std::{cell::Cell, time::Duration};

    thread_local! {
        static SUBAGENT_DISCOVERY_DEADLINE_OVERRIDE_MS: Cell<Option<u64>> = const { Cell::new(None) };
        static SUBAGENT_CAPTURE_WITHOUT_DEADLINE: Cell<bool> = const { Cell::new(false) };
        static LIVE_CAPTURE_WITHOUT_DEADLINE: Cell<bool> = const { Cell::new(false) };
        static FAIL_AFTER_SUBAGENT_CONTENT_BEFORE_PARENT_CHECKPOINT: Cell<bool> = const { Cell::new(false) };
        static INGRESS_BEFORE_REDACTION_DELAY_MS: Cell<Option<u64>> = const { Cell::new(None) };
        static LIVE_COVERAGE_AFTER_RESERVATION_DELAY_MS: Cell<Option<u64>> = const { Cell::new(None) };
    }

    tokio::task_local! {
        static TERMINAL_FINALIZER_AFTER_RESERVATION_DELAY: Option<Duration>;
    }

    /// Override the live child-discovery budget for one in-process runtime
    /// test. This is deliberately thread-local and unavailable from a hook
    /// environment, so a user-controlled process environment cannot steer a
    /// production capture deadline.
    pub(crate) struct SubagentDiscoveryDeadlineOverride {
        prior: Option<u64>,
    }

    pub(crate) fn override_subagent_discovery_deadline(
        deadline_ms: u64,
    ) -> SubagentDiscoveryDeadlineOverride {
        let prior = SUBAGENT_DISCOVERY_DEADLINE_OVERRIDE_MS
            .with(|override_ms| override_ms.replace(Some(deadline_ms.max(1))));
        SubagentDiscoveryDeadlineOverride { prior }
    }

    impl Drop for SubagentDiscoveryDeadlineOverride {
        fn drop(&mut self) {
            SUBAGENT_DISCOVERY_DEADLINE_OVERRIDE_MS.with(|override_ms| {
                override_ms.set(self.prior);
            });
        }
    }

    pub(crate) fn subagent_discovery_deadline_override_ms() -> Option<u64> {
        SUBAGENT_DISCOVERY_DEADLINE_OVERRIDE_MS.with(Cell::get)
    }

    /// The runtime unit binary is not the command binary and therefore cannot
    /// serve the production checkpoint object-I/O helper protocol. This
    /// narrowly scoped test control keeps the child writer on its direct
    /// in-process object route while exercising the real child-before-parent
    /// ordering and terminal receipt logic.
    pub(crate) struct SubagentCaptureWithoutDeadline {
        prior: bool,
    }

    pub(crate) fn capture_subagent_without_deadline() -> SubagentCaptureWithoutDeadline {
        let prior = SUBAGENT_CAPTURE_WITHOUT_DEADLINE
            .with(|without_deadline| without_deadline.replace(true));
        SubagentCaptureWithoutDeadline { prior }
    }

    impl Drop for SubagentCaptureWithoutDeadline {
        fn drop(&mut self) {
            SUBAGENT_CAPTURE_WITHOUT_DEADLINE.with(|without_deadline| {
                without_deadline.set(self.prior);
            });
        }
    }

    pub(crate) fn subagent_capture_without_deadline() -> bool {
        SUBAGENT_CAPTURE_WITHOUT_DEADLINE.with(Cell::get)
    }

    /// The library-unit executable does not provide Libra's private
    /// authorized-read entrypoint. This narrow control lets a regression
    /// exercise a completed *real* local Claude source through the coverage
    /// transaction after the snapshot phase; it does not inject a snapshot or
    /// relax the outer capture deadline used by reservation/finalization.
    pub(crate) struct LiveCaptureWithoutDeadline {
        prior: bool,
    }

    pub(crate) fn capture_live_without_deadline() -> LiveCaptureWithoutDeadline {
        let prior =
            LIVE_CAPTURE_WITHOUT_DEADLINE.with(|without_deadline| without_deadline.replace(true));
        LiveCaptureWithoutDeadline { prior }
    }

    impl Drop for LiveCaptureWithoutDeadline {
        fn drop(&mut self) {
            LIVE_CAPTURE_WITHOUT_DEADLINE.with(|without_deadline| {
                without_deadline.set(self.prior);
            });
        }
    }

    pub(crate) fn live_capture_without_deadline() -> bool {
        LIVE_CAPTURE_WITHOUT_DEADLINE.with(Cell::get)
    }

    pub(crate) fn fail_after_subagent_content_before_parent_checkpoint_once() {
        FAIL_AFTER_SUBAGENT_CONTENT_BEFORE_PARENT_CHECKPOINT.with(|armed| armed.set(true));
    }

    pub(crate) fn take_fail_after_subagent_content_before_parent_checkpoint() -> bool {
        FAIL_AFTER_SUBAGENT_CONTENT_BEFORE_PARENT_CHECKPOINT.with(|armed| armed.replace(false))
    }

    /// Deterministically advance a test hook past its initial deadline check
    /// while leaving the terminal receipt path otherwise unchanged.
    pub(crate) struct IngressBeforeRedactionDelay {
        prior: Option<u64>,
    }

    pub(crate) fn delay_ingress_before_redaction(delay_ms: u64) -> IngressBeforeRedactionDelay {
        let prior =
            INGRESS_BEFORE_REDACTION_DELAY_MS.with(|delay| delay.replace(Some(delay_ms.max(1))));
        IngressBeforeRedactionDelay { prior }
    }

    impl Drop for IngressBeforeRedactionDelay {
        fn drop(&mut self) {
            INGRESS_BEFORE_REDACTION_DELAY_MS.with(|delay| delay.set(self.prior));
        }
    }

    pub(crate) fn delay_after_ingress_before_redaction() {
        if let Some(delay_ms) = INGRESS_BEFORE_REDACTION_DELAY_MS.with(Cell::get) {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }

    /// Hold a terminal path after its primary-deadline catalog reservation
    /// but before it can bind content-free finalizer evidence. This isolates
    /// the handoff to the fixed settlement deadline from source/coverage
    /// timing, so the regression cannot pass merely because a later branch
    /// happens to recheck the clock.
    pub(crate) async fn with_terminal_finalizer_after_reservation_delay<F>(
        delay: Duration,
        future: F,
    ) -> F::Output
    where
        F: std::future::Future,
    {
        TERMINAL_FINALIZER_AFTER_RESERVATION_DELAY
            .scope(Some(delay), future)
            .await
    }

    pub(crate) fn delay_before_terminal_finalizer_after_reservation() {
        if let Ok(Some(delay)) = TERMINAL_FINALIZER_AFTER_RESERVATION_DELAY.try_with(|delay| *delay)
        {
            std::thread::sleep(delay);
        }
    }

    /// Hold a test hook after a successful live-coverage transaction but
    /// before its outcome is consumed. This proves an elapsed terminal
    /// deadline abandons acquired claims rather than acknowledging a late
    /// covered/no-op result. It is task-local test state, not a hook knob.
    pub(crate) struct LiveCoverageAfterReservationDelay {
        prior: Option<u64>,
    }

    pub(crate) fn delay_live_coverage_after_reservation(
        delay_ms: u64,
    ) -> LiveCoverageAfterReservationDelay {
        let prior = LIVE_COVERAGE_AFTER_RESERVATION_DELAY_MS
            .with(|delay| delay.replace(Some(delay_ms.max(1))));
        LiveCoverageAfterReservationDelay { prior }
    }

    impl Drop for LiveCoverageAfterReservationDelay {
        fn drop(&mut self) {
            LIVE_COVERAGE_AFTER_RESERVATION_DELAY_MS.with(|delay| delay.set(self.prior));
        }
    }

    pub(crate) fn delay_after_live_coverage_reservation() {
        if let Some(delay_ms) = LIVE_COVERAGE_AFTER_RESERVATION_DELAY_MS.with(Cell::get) {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }
}

/// External-Agent capture ingest (`refs/libra/traces`).
///
/// Consumes a command already validated and lowered by ingress, redacts
/// free-form fields, and upserts into `agent_session`. On `TurnEnd`,
/// `SessionEnd`, and subagent boundaries it also writes E4-libra checkpoint
/// commits on `refs/libra/traces`.
///
/// Boundary conditions:
/// - Idempotent on repeated `SessionStart` for the same provider session
///   (UPSERT keyed by `(agent_kind, provider_session_id)`).
/// - Best-effort on `agent_session` table absence: if the migration hasn't
///   run, returns a clear error so misconfigured installs surface a
///   diagnostic rather than panic.
pub(crate) async fn ingest_agent_traces(
    ingress_command: Box<CaptureIngressCommand>,
    binding: LiveCaptureBinding,
    ingest_span: &tracing::Span,
) -> Result<()> {
    // The ingress binding resolved this worktree once, before a command was
    // formed. Never re-read the ambient cwd here: an embedded caller can move
    // it between validation and persistence and otherwise redirect a verified
    // frame into another repository.
    let runtime_scope = ingress_command.runtime_scope().clone();
    let storage_path = runtime_scope.storage_path;
    let database_path = storage_path.join(util::DATABASE);
    // Scope/configuration discovery is read-only, but it can still block on
    // SQLite after the connection has opened.  A terminal may spend only its
    // dispatch-established settlement slice here so it can discover the
    // trusted scope needed for a content-free receipt; it never uses that
    // slice for normal catalog or checkpoint work.
    let setup_read_deadline = ingress_command
        .deadline()
        .map(|deadline| {
            if ingress_command.event_kind() == LifecycleEventKind::SessionEnd {
                hook_execution_deadline(deadline, true).map(HookExecutionDeadline::commit_deadline)
            } else {
                Ok(capture_commit_deadline(deadline))
            }
        })
        .transpose()?;
    let conn = if let Some(deadline) = ingress_command.deadline() {
        let terminal = ingress_command.event_kind() == LifecycleEventKind::SessionEnd;
        let remaining = deadline
            .monotonic()
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() && !terminal {
            bail!("capture ingress deadline expired before opening the hook database");
        }
        // An expired terminal gets one non-blocking attempt to create its
        // content-free receipt. It must not wait for a 30-second connection
        // default after the host has already reached its outer deadline.
        let busy_timeout = if remaining.is_zero() {
            // The deadline is capture's primary work window, not the host's
            // whole timeout. Give an expired terminal one short cold-start
            // connection/receipt attempt from its explicitly reserved
            // settlement slice; one millisecond is not enough to establish
            // a fresh SQLite connection even without lock contention.
            HOOK_TERMINAL_SETTLEMENT_GRACE.min(HOOK_DATABASE_BUSY_TIMEOUT_CAP)
        } else {
            remaining.min(HOOK_DATABASE_BUSY_TIMEOUT_CAP)
        };
        // A database-open failure keeps only its closed, path-free class so
        // fail-closed hook surfaces can restore the preflight's
        // `LBR-REPO-002` (missing) / `LBR-IO-001` (unopenable) contract.
        let database_path = database_path.to_str().ok_or_else(|| {
            active_repository_failure(ActiveRepositoryFailureClass::DatabaseUnavailable)
        })?;
        // The managed open migrates and fences exactly like every repository
        // command (and like the no-deadline branch below): pending schema
        // migrations are applied first, one claim-first transaction per
        // version, and a schema written by a newer Libra is refused, which
        // the closed class renders as the path-free `LBR-IO-001`
        // newer-binary remedy. Only the lock wait is bounded by the hook's
        // database slice; migration DDL itself is not pre-empted.
        db::establish_connection_with_busy_timeout(database_path, busy_timeout)
            .await
            .map_err(|err| {
                active_repository_failure(ActiveRepositoryFailureClass::for_database_open(
                    err.kind(),
                ))
            })?
    } else {
        db::get_db_conn_instance_for_path(&database_path)
            .await
            .map_err(|err| {
                active_repository_failure(ActiveRepositoryFailureClass::for_database_open(
                    err.kind(),
                ))
            })?
    };
    await_agent_traces_pure_read_until(
        setup_read_deadline,
        "failed to configure hash kind from repo config",
        set_hash_kind_from_connection(&conn),
    )
    .await?;
    let repo_root = runtime_scope.worktree_root;
    let scope = await_agent_traces_pure_read_until(
        setup_read_deadline,
        "resolve capture scope from hook database",
        CaptureScope::resolve(&conn, &repo_root),
    )
    .await?;
    if ingress_command.event_kind() == LifecycleEventKind::SessionStart {
        match await_agent_traces_pure_read_until(
            setup_read_deadline,
            "inspect bounded capture recovery hint",
            crate::internal::ai::capture::pending::has_pending_hint(&conn, &scope),
        )
        .await
        {
            Ok(true) => {
                tracing::info!(
                    target: "agent.capture.recovery",
                    reason = "pending_artifact_hint",
                    "a bounded local capture recovery may be available"
                );
                if crate::internal::ai::capture::worker::spawn_detached(&storage_path, &repo_root)
                    .is_err()
                {
                    tracing::warn!(
                        target: "agent.capture.recovery",
                        reason = "worker_spawn_failed",
                        "automatic local recovery could not start; run `libra agent doctor --repair`"
                    );
                }
            }
            Ok(false) => {}
            Err(_) => tracing::warn!(
                target: "agent.capture.recovery",
                reason = "pending_hint_unavailable",
                "could not inspect the bounded local capture recovery hint; run `libra agent doctor`"
            ),
        }
    }

    ingest_agent_traces_payload_with_scope(
        ingress_command,
        binding,
        &conn,
        Some(&storage_path),
        &scope,
        ingest_span,
        &ExportJobLeaseStore::new(&conn),
    )
    .await
}

/// Consume an already-lowered in-process ingress outcome. This exists solely
/// to let span/integration tests drive the same typed runtime as production;
/// unlike the retired helper it never accepts a raw hook frame.
pub(crate) async fn ingest_agent_traces_ingress_outcome_for_test(
    ingress: CaptureIngressOutcome,
    command: ProviderHookCommand,
    provider: &dyn HookProvider,
    binding: LiveCaptureBinding,
    conn: &sea_orm::DatabaseConnection,
    repo_path: Option<&std::path::Path>,
) -> Result<()> {
    let ingest_span = new_ingest_span(command, provider);
    match ingress {
        CaptureIngressOutcome::Command(ingress_command) => {
            ingest_span.record("validated", true);
            ingest_span.record("frame_bytes", ingress_command.frame_bytes() as u64);
            let scope = CaptureScope::main_for_connection(conn).await?;
            ingest_agent_traces_payload_with_scope(
                ingress_command,
                binding,
                conn,
                repo_path,
                &scope,
                &ingest_span,
                &ExportJobLeaseStore::new(conn),
            )
            .await
        }
        CaptureIngressOutcome::UnknownEvent {
            frame_bytes,
            event_name_len,
        } => {
            ingest_span.record("validated", true);
            ingest_span.record("frame_bytes", frame_bytes as u64);
            ingest_span.record("partial", true);
            let _entered = ingest_span.enter();
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = provider.provider_name(),
                event_name_len,
                reason = "unknown_event_type",
                "skipping unrecognized lifecycle event name"
            );
            Ok(())
        }
    }
}

/// Settle an already-expired terminal callback with the smallest durable
/// recovery path. At this point ingress has validated the frame and bound its
/// worktree scope, while the caller has opened the scoped database connection.
/// Do not add redaction, source, coverage, checkpoint, or maintenance-lock
/// work here: this branch exists so a short host timeout still leaves one
/// content-free, replayable terminal receipt.
struct ExpiredTerminalContext<'a> {
    conn: &'a sea_orm::DatabaseConnection,
    scope: &'a CaptureScope,
    // The ordinary and expired terminal paths share one binding, so their
    // scope-fenced catalog identity cannot drift.
    binding: LiveCaptureBinding,
    envelope: &'a SessionHookEnvelope,
    event_kind: LifecycleEventKind,
    event_id: uuid::Uuid,
    dedup_key: Option<&'a str>,
    // Established before any runtime work from the immutable dispatch-time
    // pair. This is the sole grace budget for a content-free terminal
    // receipt; it is never used for source, checkpoint, or ordinary catalog
    // lifecycle work.
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
    ingest_span: &'a tracing::Span,
}

async fn persist_expired_terminal_pending(
    context: &ExpiredTerminalContext<'_>,
    deadline: CaptureDeadline,
) -> Result<()> {
    // This recovery path gets only the dispatch-established fixed grace
    // window.  Even its scope/state reads must not sit behind a writer after
    // the normal capture deadline has elapsed.
    let settlement_deadline = context.terminal_settlement_deadline.ok_or_else(|| {
        anyhow!("expired terminal capture lost its established settlement deadline")
    })?;
    await_agent_traces_pure_read_until(
        Some(settlement_deadline),
        "validate provider-session workspace ownership for expired terminal",
        context
            .scope
            .assert_provider_session_compatible(context.conn, &context.envelope.session_id),
    )
    .await?;
    let agent_kind = context.binding.agent_kind_db();
    let now = Utc::now().timestamp();
    let current_capture_state = await_agent_traces_pure_read_until(
        Some(settlement_deadline),
        "read durable agent capture state while settling expired terminal",
        load_capture_state(
            context.conn,
            agent_kind,
            &context.envelope.session_id,
            context.scope,
        ),
    )
    .await?;
    // A fully completed terminal state has no recovery work left. Creating a
    // fresh pending receipt here would strand it because finalizer recovery
    // intentionally scans only non-stopped sessions; a later native delivery
    // must therefore remain an explicit durable no-op on this fast path.
    if current_capture_state
        .is_some_and(|state| state.phase == CapturePhase::Stopped && state.stopped_at.is_some())
    {
        return Ok(());
    }
    let action_plan = reduce_lifecycle(LifecycleReducerInput {
        current: current_capture_state,
        event_kind: LifecycleEventKind::SessionEnd,
        event_id: context.event_id,
        occurred_at: now,
        deadline: Some(deadline.absolute_millis().div_euclid(1_000)),
    })
    .context("reduce expired terminal capture lifecycle action")?;
    let catalog_session = CaptureCatalogSession::new(
        build_ai_session_id(context.binding.hook_name(), &context.envelope.session_id),
        agent_kind,
        &context.envelope.session_id,
        &context.envelope.cwd,
    )
    .map_err(|error| anyhow!("prepare expired terminal capture catalog session: {error}"))?;
    let catalog_action = CaptureCatalogAction::from_ingress(
        context.event_id,
        context.dedup_key,
        LifecycleEventKind::SessionEnd,
    )
    .map_err(|error| anyhow!("prepare expired terminal capture receipt: {error}"))?;
    let catalog_mutation =
        CaptureCatalogMutation::from_reducer(current_capture_state, &action_plan, now)
            .map_err(|error| anyhow!("prepare expired terminal capture mutation: {error}"))?;
    let catalog_request = CaptureCatalogApplyRequest::new(
        context.scope.clone(),
        catalog_session,
        catalog_action,
        catalog_mutation,
    )
    .map_err(|error| anyhow!("prepare expired terminal scope-fenced catalog request: {error}"))?
    .with_metadata(CaptureCatalogMetadataPatch::default());
    // This recovery path is the explicit terminal-settlement exception. It
    // gets only the dispatch-established fixed grace window and cannot reuse
    // the ordinary expired capture budget for new snapshot/checkpoint work.
    let reservation = match reserve_capture_catalog_until(
        context.conn.clone(),
        &catalog_request,
        Some(settlement_deadline),
    )
    .await
    .map_err(|error| anyhow!("reserve expired terminal catalog mutation: {error}"))?
    {
        CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
        CaptureCoordinatorReserveOutcome::AlreadyApplied => return Ok(()),
        CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict } => {
            bail!(
                "expired terminal catalog precondition changed before recovery receipt: {conflict:?}"
            );
        }
    };
    let effective_request = reservation.effective_request().clone();
    settle_preapplied_without_checkpoint(
        context.conn,
        reservation,
        &effective_request,
        Some(deadline.absolute_millis()),
        None,
    )
    .await?;
    context.ingest_span.record("partial", true);
    let _entered = context.ingest_span.enter();
    tracing::warn!(
        target: "agent.hook.ingest",
        provider = context.binding.hook_name(),
        reason = "expired_terminal_pending_receipt",
        "capture host deadline elapsed; persisted content-free terminal recovery receipt"
    );
    Ok(())
}

/// Recheck a managed terminal deadline at every boundary before expensive
/// work. A timeout can elapse after initial ingress while redaction or a
/// scoped SQLite read is in flight; once observed, discard the remaining
/// content path and settle only the content-free recovery receipt.
async fn settle_expired_terminal_if_needed(
    deadline: Option<CaptureDeadline>,
    context: &ExpiredTerminalContext<'_>,
) -> Result<bool> {
    let Some(deadline) = deadline else {
        return Ok(false);
    };
    if context.event_kind != LifecycleEventKind::SessionEnd || Instant::now() < deadline.monotonic()
    {
        return Ok(false);
    }
    persist_expired_terminal_pending(context, deadline).await?;
    Ok(true)
}

/// Apply the primary deadline to a normal pure read.  If that deadline is
/// observed on a trusted terminal, discard the read result and use the
/// existing content-free settlement route; nonterminal failures keep their
/// normal error behavior.
async fn await_agent_traces_primary_read_or_settle_terminal<T>(
    deadline: Option<CaptureDeadline>,
    context: &ExpiredTerminalContext<'_>,
    operation: &'static str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<T>> {
    match await_agent_traces_pure_read_until(deadline.map(capture_commit_deadline), operation, read)
        .await
    {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_agent_traces_read_deadline(&error) => {
            if settle_expired_terminal_if_needed(deadline, context).await? {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

async fn ingest_agent_traces_payload_with_scope(
    ingress_command: Box<CaptureIngressCommand>,
    binding: LiveCaptureBinding,
    conn: &sea_orm::DatabaseConnection,
    repo_path: Option<&std::path::Path>,
    scope: &CaptureScope,
    ingest_span: &tracing::Span,
    export_lease: &impl LiveExportLeasePort,
) -> Result<()> {
    use crate::internal::ai::observed_agents::{RedactionMatch, Redactor};

    let crate::internal::ai::capture::ingress::CaptureIngressParts {
        hook_command: _command,
        provider_kind,
        provider_source,
        frame_bytes,
        event_id,
        identity_scheme,
        deadline,
        dedup_key,
        context,
        event,
    } = (*ingress_command).into_parts();
    // The current caller resolves its binding from the same provider used to
    // build ingress. Keep this invariant explicit while the later coordinator
    // takes ownership of the full trusted handoff.
    debug_assert_eq!(provider_kind, binding.hook_name());
    ingest_span.record("frame_bytes", frame_bytes as u64);
    let (envelope, _runtime_scope) = narrow_envelope_from_capture_context(context, event.kind);
    // A provider without an installer-provided managed budget (its exporter
    // owns the window) gets that budget as the single capture deadline before
    // any durable runtime path can begin. Do not construct another deadline
    // inside the export branch below: every downstream operation must retain
    // this exact pair.
    let deadline = effective_capture_deadline(binding.missing_host_capture_budget(), deadline)?;
    // Command dispatch established both deadline clocks before any hook
    // configuration or storage work. Preserve that original wall-clock
    // commitment in terminal receipts; runtime must never re-anchor it.
    let capture_finalizer_deadline_millis = deadline.map(CaptureDeadline::absolute_millis);
    let capture_deadline = deadline.map(capture_commit_deadline);
    // A terminal receipt gets one fixed, dispatch-derived settlement slice.
    // Establish it before any runtime I/O so a later timeout cannot borrow a
    // new wall-clock allowance. It is passed only to the coordinator's
    // content-free pending paths below; covered-terminal replay, the initial
    // lifecycle reservation, and every checkpoint continue to use
    // `capture_deadline`.
    let terminal_settlement_deadline = if event.kind == LifecycleEventKind::SessionEnd {
        deadline
            .map(|deadline| {
                hook_execution_deadline(deadline, true).map(HookExecutionDeadline::commit_deadline)
            })
            .transpose()?
    } else {
        None
    };
    let lifecycle_deadline_seconds =
        capture_finalizer_deadline_millis.map(|millis| millis.div_euclid(1_000));
    let expired_terminal_context = ExpiredTerminalContext {
        conn,
        scope,
        binding,
        envelope: &envelope,
        event_kind: event.kind,
        event_id,
        dedup_key: dedup_key.as_deref(),
        terminal_settlement_deadline,
        ingest_span,
    };
    if settle_expired_terminal_if_needed(deadline, &expired_terminal_context).await? {
        return Ok(());
    }
    if event.kind != LifecycleEventKind::SessionEnd
        && capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic())
    {
        bail!("capture ingress deadline expired before runtime processing");
    }
    ingest_span.record("event_id", tracing::field::display(event_id));
    ingest_span.record("provider_source", provider_source);
    let mut event = event;
    ingest_span.record("event_kind", tracing::field::display(event.kind));
    ingest_span.record("partial", false);

    #[cfg(test)]
    test_support::delay_after_ingress_before_redaction();

    // Redact every permitted free-form text field before it gets anywhere
    // near durable storage (AG-19 redaction-before-persist: prompt, tool
    // input/response, and assistant message). Provider-reported model,
    // source, tool-name, and session-reference fields are not needed for a
    // generic capture checkpoint and cannot be safely classified by a
    // pattern redactor, so drop them instead of preserving arbitrary text.
    // We aggregate the
    // per-field reports into a single JSON document that lands in
    // `agent_session.redaction_report` so the persisted row is observably
    // scrubbed (Codex round-3 review: "assert observable redaction
    // outcome").
    let redaction_span = tracing::info_span!(
        "agent.redaction.apply",
        rules_hit = tracing::field::Empty,
        size_cap_triggered = false,
        fail_closed = false,
    );
    let (all_matches, bytes_scanned, bytes_redacted, dropped_matches) =
        redaction_span.in_scope(|| {
            let redactor = Redactor::new_default();
            let mut all_matches: Vec<RedactionMatch> = Vec::new();
            let mut bytes_scanned: usize = 0;
            let mut bytes_redacted: usize = 0;
            let mut dropped_matches: usize = 0;
            let mut redact_string = |value: &mut Option<String>| {
                if let Some(text) = value.take() {
                    let (redacted, report) = redactor.redact(text.as_bytes());
                    *value = Some(String::from_utf8_lossy(redacted.bytes()).into_owned());
                    bytes_scanned += report.bytes_scanned;
                    bytes_redacted += report.bytes_redacted;
                    append_redaction_report_bounded(&mut all_matches, &mut dropped_matches, report);
                }
            };
            redact_string(&mut event.prompt);
            redact_string(&mut event.assistant_message);
            event.model = None;
            event.source = None;
            event.tool_name = None;
            event.session_ref = None;
            let mut redact_value = |value: &mut Option<serde_json::Value>| {
                if let Some(inner) = value.take() {
                    let Ok(serialized) = serde_json::to_vec(&inner) else {
                        // A value which cannot round-trip through the redactor is
                        // unsafe to preserve. Drop it rather than silently retain
                        // unredacted provider bytes.
                        return;
                    };
                    let (redacted, report) = redactor.redact(&serialized);
                    *value = serde_json::from_slice(redacted.bytes()).ok();
                    bytes_scanned += report.bytes_scanned;
                    bytes_redacted += report.bytes_redacted;
                    append_redaction_report_bounded(&mut all_matches, &mut dropped_matches, report);
                }
            };
            redact_value(&mut event.tool_input);
            redact_value(&mut event.tool_response);
            (all_matches, bytes_scanned, bytes_redacted, dropped_matches)
        });
    redaction_span.record(
        "rules_hit",
        all_matches.len().saturating_add(dropped_matches) as u64,
    );
    let runtime_redaction_report = crate::internal::ai::observed_agents::RedactionReport {
        matches: all_matches.clone(),
        dropped_matches,
        bytes_scanned,
        bytes_redacted,
    };
    let mut redaction_report_value = serde_json::json!({
        "matches": all_matches,
        "bytes_scanned": bytes_scanned,
        "bytes_redacted": bytes_redacted,
    });
    if dropped_matches != 0
        && let Some(object) = redaction_report_value.as_object_mut()
    {
        object.insert("dropped_matches".to_string(), json!(dropped_matches));
    }
    let redaction_report_json = serde_json::to_string(&redaction_report_value)
        .context("serialize sanitized agent redaction report")?;

    if settle_expired_terminal_if_needed(deadline, &expired_terminal_context).await? {
        return Ok(());
    }

    let backend = conn.get_database_backend();

    // If the migration has not run yet, fail loud rather than silently.
    let table_check = match await_agent_traces_primary_read_or_settle_terminal(
        deadline,
        &expired_terminal_context,
        "failed to query sqlite_master",
        agent_session_table_exists(conn, backend),
    )
    .await?
    {
        Some(table_check) => table_check,
        None => return Ok(()),
    };
    if table_check.is_none() {
        bail!(
            "agent_session table does not exist; run `libra init` against this repository to apply migrations",
        );
    }
    if await_agent_traces_primary_read_or_settle_terminal(
        deadline,
        &expired_terminal_context,
        "validate provider-session workspace ownership",
        scope.assert_provider_session_compatible(conn, &envelope.session_id),
    )
    .await?
    .is_none()
    {
        return Ok(());
    }
    if settle_expired_terminal_if_needed(deadline, &expired_terminal_context).await? {
        return Ok(());
    }

    let now = Utc::now().timestamp();
    let session_id = build_ai_session_id(binding.hook_name(), &envelope.session_id);
    let agent_kind = binding.agent_kind_db();

    // AG-19 owner filtering: first-writer-wins by recorded owner agent
    // kind per provider session id, so two adapters forwarding the same
    // underlying session cannot double-write checkpoints. SessionStart /
    // TurnStart are exempt (they may establish a claim); every other
    // event from a non-owner agent kind is skipped-and-logged, never a
    // hard error (`agent.md` AG-19 owner-filtering row).
    //
    // Ownership is `rowid ASC` — true insertion order. `agent_session` is
    // an ordinary rowid table and the UPSERT preserves rowids, so the
    // first physically-inserted claim wins permanently. The previous
    // `(started_at ASC, session_id ASC)` ordering used second-granularity
    // timestamps: a later-arriving row inserted within the same second
    // could win the lexicographic tiebreak AFTER the earlier row's owner
    // had already confirmed and written a checkpoint, yielding
    // checkpoints from two agent kinds for one provider session (caught
    // by `agent_lifecycle_event_test::
    // simultaneous_stop_race_yields_single_owner_checkpoints`).
    if !matches!(
        event.kind,
        LifecycleEventKind::SessionStart | LifecycleEventKind::TurnStart
    ) {
        let owner_row = match await_agent_traces_primary_read_or_settle_terminal(
            deadline,
            &expired_terminal_context,
            "failed to query agent_session owner claim",
            scoped_owner_agent_kind(
                conn,
                backend,
                &envelope,
                scope,
                "query agent_session owner claim",
            ),
        )
        .await?
        {
            Some(owner_row) => owner_row,
            None => return Ok(()),
        };
        if let Some(row) = owner_row {
            let owner_kind: String = row
                .try_get("", "agent_kind")
                .context("failed to read agent_session owner kind")?;
            if owner_kind != agent_kind {
                ingest_span.record("partial", true);
                let _entered = ingest_span.enter();
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = binding.hook_name(),
                    owner = %owner_kind,
                    event_kind = %event.kind,
                    reason = "owner_mismatch",
                    "skipping non-owner lifecycle event (first-writer-wins)"
                );
                return Ok(());
            }
        }
    }

    // Lifecycle/checkpoint decisions remain provider-neutral and pure. The
    // two read-only queries here supply the reducer and owner gate; the
    // catalog is the sole durable `agent_session` mutation boundary.
    let current_capture_state = match await_agent_traces_primary_read_or_settle_terminal(
        deadline,
        &expired_terminal_context,
        "read durable agent capture state",
        load_capture_state(conn, agent_kind, &envelope.session_id, scope),
    )
    .await?
    {
        Some(current_capture_state) => current_capture_state,
        None => return Ok(()),
    };
    if settle_expired_terminal_if_needed(deadline, &expired_terminal_context).await? {
        return Ok(());
    }
    let preexisting_durable_terminal = current_capture_state
        .is_some_and(|state| state.phase == CapturePhase::Stopped && state.stopped_at.is_some());
    let action_plan = reduce_lifecycle(LifecycleReducerInput {
        current: current_capture_state,
        event_kind: event.kind,
        event_id,
        occurred_at: now,
        deadline: lifecycle_deadline_seconds,
    })
    .context("reduce agent capture lifecycle action")?;
    let concurrent_active = match await_agent_traces_primary_read_or_settle_terminal(
        deadline,
        &expired_terminal_context,
        "count concurrent active agent sessions",
        session_concurrent_active(conn, backend, event.kind, &session_id, &envelope.cwd, scope),
    )
    .await?
    {
        Some(concurrent_active) => concurrent_active,
        None => return Ok(()),
    };
    let catalog_session = CaptureCatalogSession::new(
        session_id.clone(),
        agent_kind,
        envelope.session_id.clone(),
        envelope.cwd.clone(),
    )
    .map_err(|error| anyhow!("prepare capture catalog session: {error}"))?;
    let catalog_action =
        CaptureCatalogAction::from_ingress(event_id, dedup_key.as_deref(), event.kind)
            .map_err(|error| anyhow!("prepare capture catalog receipt: {error}"))?;
    let catalog_mutation =
        CaptureCatalogMutation::from_reducer(current_capture_state, &action_plan, now)
            .map_err(|error| anyhow!("prepare capture catalog mutation: {error}"))?;
    let catalog_request = CaptureCatalogApplyRequest::new(
        scope.clone(),
        catalog_session,
        catalog_action,
        catalog_mutation,
    )
    .map_err(|error| anyhow!("prepare scope-fenced capture catalog request: {error}"))?
    .with_metadata(CaptureCatalogMetadataPatch::new(
        concurrent_active,
        Some(CaptureCatalogRedactionReport::from_report(
            &runtime_redaction_report,
        )),
    ));
    if settle_expired_terminal_if_needed(deadline, &expired_terminal_context).await? {
        return Ok(());
    }
    // Reserve through the coordinator boundary before performing expensive
    // snapshot/coverage work. The returned reservation is later handed back
    // to its opaque coordinator token, which owns checkpoint write + receipt
    // completion; runtime cannot call the catalog mutation port directly.
    let catalog_reservation = match reserve_capture_catalog_until(
        conn.clone(),
        &catalog_request,
        capture_deadline,
    )
    .await
    .map_err(|error| anyhow!("reserve scope-fenced capture catalog mutation: {error}"))?
    {
        CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
        CaptureCoordinatorReserveOutcome::AlreadyApplied => return Ok(()),
        CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict } => {
            ingest_span.record("partial", true);
            // A validated terminal boundary refused because this provider
            // session is already recorded under another working directory
            // (or local id) left no receipt, artifact, or worker trigger.
            // Acknowledging it would silently lose the terminal boundary
            // (docs/commands/hooks.md), so surface an explicit content-free
            // incomplete outcome instead. The cross-provider owner race
            // also reports `SessionIdentity`; that loser is not this
            // session's boundary and keeps its harmless acknowledgement.
            if catalog_request.mutation().is_terminal()
                && conflict == CaptureCatalogConflict::SessionIdentity
                && terminal_session_identity_changed(
                    conn,
                    capture_deadline,
                    agent_kind,
                    &envelope,
                    &session_id,
                )
                .await?
            {
                let _entered = ingest_span.enter();
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = binding.hook_name(),
                    event_kind = %event.kind,
                    reason = "terminal_session_identity_conflict",
                    capture_outcome = "incomplete",
                    "terminal capture is incomplete because the recorded session identity differs; no recovery artifact was retained"
                );
                bail!(
                    "capture terminal boundary is incomplete: this provider session is recorded \
                         under a different working directory or local session, so no checkpoint or \
                         recovery artifact was retained; end the session from its original working \
                         directory or inspect it with `libra agent doctor`"
                );
            }
            let _entered = ingest_span.enter();
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = binding.hook_name(),
                event_kind = %event.kind,
                reason = "catalog_conflict",
                catalog_conflict = ?conflict,
                "skipping lifecycle event because its catalog precondition changed"
            );
            return Ok(());
        }
    };
    // An ID-less terminal redelivery can atomically adopt the prior local
    // pending receipt inside the catalog transaction. From here on every
    // checkpoint, finalizer, and canonical lifecycle record must use that
    // effective action identity rather than the fresh ingress UUID.
    let catalog_request = catalog_reservation.effective_request().clone();
    if catalog_request.mutation().is_terminal()
        && capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic())
    {
        return settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            &catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await;
    }
    let event_id = catalog_request.action().event_id();
    #[cfg(test)]
    crate::internal::ai::capture::test_support::delete_reserved_session_if_armed(conn, &session_id)
        .await?;
    let checkpoint = catalog_reservation.checkpoint_kind();
    let receipt_pending = catalog_reservation.receipt_is_pending();
    if receipt_pending && checkpoint == CheckpointWrite::None {
        bail!("capture catalog returned a pending receipt without checkpoint work");
    }
    // The initial reservation uses the primary paired deadline because it
    // advances lifecycle state. If that budget elapsed while the reservation
    // was being handed to the terminal finalizer, abandon that primary token
    // and resume only the pending receipt through the fixed settlement path.
    // In particular, do not let `claim_terminal_attempt` observe the elapsed
    // primary deadline and strand a bare terminal receipt.
    if checkpoint != CheckpointWrite::None
        && catalog_request.mutation().is_terminal()
        && capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic())
    {
        return settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            &catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await;
    }
    // A terminal receipt must gain durable finalizer evidence before any
    // fallible lock, source, coverage, or object work. Otherwise a failure
    // in those preparatory stages leaves a bare pending receipt that a later
    // `ResumePending` cannot strictly complete. The provisional marker is
    // intentionally content-free; the coordinator binds it to a real marker
    // and source only when a checkpoint attempt is ready.
    if checkpoint != CheckpointWrite::None && catalog_request.mutation().is_terminal() {
        // This deliberately sits after the elapsed-primary check above. The
        // test seam exercises the remaining race where the finalizer's own
        // database authorization observes expiry after that check.
        #[cfg(test)]
        test_support::delay_before_terminal_finalizer_after_reservation();
        let policy = CaptureFinalizePolicy::new(
            capture_finalizer_deadline_millis,
            CaptureFinalizeMode::Deferrable,
            catalog_request.action().action_key(),
        )
        .map_err(|error| anyhow!("prepare terminal capture finalizer policy: {error}"))?;
        let finalizer_outcome = match catalog_reservation
            .prepare_terminal_finalizer(
                &catalog_request,
                CaptureCoordinatorFinalizer::new(policy, finalizer_now_millis()),
            )
            .await
        {
            Ok(outcome) => outcome,
            // The primary deadline can expire after the recheck above but
            // while the finalizer reaches its final database authorization.
            // Only that classified failure may resume the already-applied
            // terminal receipt with the fixed, dispatch-established grace
            // pair; every other finalizer failure remains fail-closed.
            Err(CaptureCoordinatorError::CatalogFinalize(
                CaptureCatalogError::DeadlineExceeded,
            )) if terminal_settlement_deadline.is_some() => {
                return settle_preapplied_without_checkpoint(
                    conn,
                    catalog_reservation,
                    &catalog_request,
                    capture_finalizer_deadline_millis,
                    terminal_settlement_deadline,
                )
                .await;
            }
            Err(error) => {
                return Err(anyhow!(
                    "prepare terminal capture finalizer receipt: {error}"
                ));
            }
        };
        match finalizer_outcome {
            CaptureCoordinatorOutcome::FinalizerPending { .. }
            | CaptureCoordinatorOutcome::AlreadyApplied => {}
            CaptureCoordinatorOutcome::FinalizerQuarantined { reason } => {
                bail!("capture terminal finalizer requires repair: {reason:?}");
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                bail!("capture terminal finalizer lost its catalog fence: {conflict:?}");
            }
            outcome => bail!(
                "capture coordinator returned an invalid terminal preparation outcome: {outcome:?}"
            ),
        }
    }
    if checkpoint == CheckpointWrite::None {
        // Even no-checkpoint lifecycle actions pass through the concrete
        // coordinator. This consumes the opaque catalog reservation and
        // prevents a runtime-only fast path from bypassing reducer receipt
        // semantics as capture grows additional state-only actions.
        let coordinator_request =
            CaptureCoordinatorRequest::new(catalog_request, None).map_err(|error| {
                anyhow!("prepare no-checkpoint capture coordinator request: {error}")
            })?;
        let coordinator: CaptureCoordinator<_, _> =
            catalog_reservation.into_coordinator(NoCheckpointStore);
        match coordinator
            .execute_preapplied(coordinator_request)
            .await
            .map_err(|error| anyhow!("execute no-checkpoint capture coordinator: {error}"))?
        {
            CaptureCoordinatorOutcome::StateApplied | CaptureCoordinatorOutcome::AlreadyApplied => {
                return Ok(());
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                bail!("no-checkpoint capture coordinator lost its catalog fence: {conflict:?}");
            }
            outcome => {
                bail!("no-checkpoint capture coordinator returned an invalid outcome: {outcome:?}");
            }
        }
    }
    let mut catalog_reservation = Some(catalog_reservation);

    // entire.md §6.3 state machine: both `TurnEnd` (Stop — end of a turn,
    // session stays `active`) and `SessionEnd` (final) materialise a
    // `committed` checkpoint commit on `refs/libra/traces`, indexed in
    // `agent_checkpoint`. The checkpoint's tree carries metadata.json + the
    // redacted transcript blob (now the agent's full on-disk transcript, see
    // the writer); events-blob inclusion remains a follow-up. Per-turn
    // checkpoints give `libra agent checkpoint rewind` turn-level granularity.
    if checkpoint != CheckpointWrite::None
        && let Some(repo) = repo_path
    {
        // This ID is derived only from the canonical ingress identity and
        // checkpoint class. A catalog `ResumePending` must reuse it so a
        // retry can discover the already durable checkpoint before it builds
        // objects or advances the traces ref again.
        let checkpoint_id = checkpoint_id_for_capture_action(event_id, checkpoint);
        // AG-19 owner-race closure: the pre-upsert owner check above is a
        // fast path, but two providers racing on a fresh provider session
        // can BOTH pass it (each sees no owner) and both upsert. Re-read
        // the claim now that our row is durably in place. Ordering by
        // `rowid ASC` makes this confirmation *monotone*: an existing
        // row's rowid never changes and no later insert can obtain a
        // smaller one, so once a racer confirms itself as owner no
        // subsequently-arriving row can flip the answer — exactly one of
        // the racers writes the checkpoint. The loser keeps its metadata
        // row but is skipped fail-closed here.
        let confirmed_owner: String = match await_agent_traces_pure_read_until(
            capture_deadline,
            "failed to re-confirm agent_session owner claim",
            async {
                scoped_owner_agent_kind(
                    conn,
                    backend,
                    &envelope,
                    scope,
                    "query post-reservation agent_session owner claim",
                )
                .await?
                .map(|row| row.try_get("", "agent_kind"))
                .transpose()
                .context("failed to read confirmed agent_session owner kind")?
                .ok_or_else(|| {
                    anyhow!(
                        "capture session disappeared after its catalog reservation; retry the hook from the current provider session"
                    )
                })
            },
        )
        .await
        {
            Ok(confirmed_owner) => confirmed_owner,
            // The catalog reservation has already recorded terminal recovery
            // evidence.  A read timeout here must not leave the reservation
            // stranded or extend the primary deadline into checkpoint work.
            Err(error)
                if is_agent_traces_read_deadline(&error)
                    && catalog_request.mutation().is_terminal() =>
            {
                return settle_preapplied_without_checkpoint(
                    conn,
                    catalog_reservation.take().ok_or_else(|| {
                        anyhow!("capture coordinator reservation was already consumed")
                    })?,
                    &catalog_request,
                    capture_finalizer_deadline_millis,
                    terminal_settlement_deadline,
                )
                .await;
            }
            Err(error) => return Err(error),
        };
        if confirmed_owner != agent_kind {
            ingest_span.record("partial", true);
            let _entered = ingest_span.enter();
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = binding.hook_name(),
                owner = %confirmed_owner,
                event_kind = %event.kind,
                reason = "owner_mismatch",
                "detected an unexpected post-reservation owner mismatch"
            );
            // `CaptureCatalogStore::apply_inner` fences this ownership inside
            // its write transaction, before any receipt can be reserved. If
            // this defensive re-check ever differs, do not acknowledge a
            // terminal event or manufacture a finalizer from a loser row.
            bail!(
                "capture owner changed after its catalog reservation; retry from the owning provider session"
            );
        }
        // Codex hooks skip the generic command preflight so an auxiliary
        // callback can be acknowledged when storage is unavailable. Take the
        // shared hold at the actual object-publication boundary instead.
        let _maintenance_lock = match crate::internal::maintenance_lock::MaintenanceLock::shared(
            repo,
        )
        .context(
            "failed to acquire the repository maintenance lock before writing an agent checkpoint",
        ) {
            Ok(lock) => lock,
            Err(error) => {
                // A pending catalog receipt owns the session revision. Do
                // not let the legacy best-effort diagnostic update advance
                // that revision, or its terminal completion would be fenced
                // before a retry can resume it.
                if !receipt_pending {
                    record_live_maintenance_lock_failure_until(
                        conn.clone(),
                        &catalog_request,
                        capture_deadline,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        // A0-02: `SubagentStart` / `SubagentEnd` boundaries materialise an
        // independent `scope='subagent'` checkpoint (its own `traces`
        // commit + `agent_checkpoint` row) that carries parent session /
        // checkpoint linkage, so `checkpoint list/show/export/prune` and
        // `doctor` surface nested runs as first-class checkpoints instead of
        // leaving them as bounded `subagent_events` metadata on the main
        // checkpoint. `SessionEnd` / `TurnEnd` keep the `committed` path.
        if checkpoint == CheckpointWrite::SubagentBoundary {
            write_subagent_checkpoint(
                conn,
                repo,
                catalog_request.clone(),
                catalog_reservation.take().ok_or_else(|| {
                    anyhow!("capture coordinator reservation was already consumed")
                })?,
                &session_id,
                &envelope,
                agent_kind,
                &event,
                event_id,
                identity_scheme,
                &checkpoint_id,
                &redaction_report_json,
                now,
                scope,
                capture_deadline,
            )
            .await?;
        } else {
            let subagent_deadline_ms = 8_000;
            #[cfg(test)]
            let subagent_deadline_ms = test_support::subagent_discovery_deadline_override_ms()
                .unwrap_or(subagent_deadline_ms);
            // Establish the child capture deadline in both clock domains at
            // the same dispatch boundary.  In particular, do not derive a
            // fresh SQLite deadline from the parent's remaining monotonic
            // time: a child that inherits the parent limit must retain the
            // parent's original final-commit authorization fence.
            let local_subagent_capture_deadline =
                CaptureCommitDeadline::from_budget(Duration::from_millis(subagent_deadline_ms))
                    .context("establish bounded subagent hook capture deadline")?;
            // Child discovery/capture is auxiliary to the parent checkpoint.
            // For a managed hook, reserve part of the remaining host window
            // for the parent before applying the local eight-second cap. A
            // terminal callback whose host window already elapsed skips child
            // work entirely and remains explicitly partial rather than
            // borrowing a fresh child budget after finalization is due.
            let subagent_capture_deadline = match capture_deadline {
                Some(host_deadline)
                    if host_deadline.monotonic() <= local_subagent_capture_deadline.monotonic() =>
                {
                    Some(host_deadline)
                }
                _ => Some(local_subagent_capture_deadline),
            };
            let subagent_discovery_deadline = subagent_capture_deadline.and_then(|deadline| {
                crate::internal::ai::subagent_content::discovery_deadline_preserving_parent(
                    deadline.monotonic(),
                )
                .ok()
            });
            #[cfg(test)]
            let subagent_capture_deadline = (!test_support::subagent_capture_without_deadline())
                .then_some(subagent_capture_deadline)
                .flatten();
            let live_context = LiveCaptureContext {
                libra_session_id: &session_id,
                provider_session_id: &envelope.session_id,
                verified_cwd: std::path::Path::new(&envelope.cwd),
            };
            let mut subagent_discovery =
                discover_subagents(binding, &live_context, subagent_discovery_deadline).await;
            // Child content must be durable before a parent advertises child-derived
            // files/usage/source counts. A retry can safely no-op these source leaves
            // if the subsequent parent append fails.
            if let Err(error) = crate::internal::ai::subagent_content::capture_discovered_subagent_contents_with_scope(
                conn,
                scope,
                repo,
                &session_id,
                &subagent_discovery.sources,
                "live",
                subagent_capture_deadline,
            )
            .await
            {
                if crate::internal::ai::subagent_content::capture_deadline_exhausted(&error) {
                    tracing::warn!(
                        session_id = %redact_session_id(&session_id),
                        reason = "bounded_subagent_capture_deadline",
                        "child capture reached its reserved slice; preserving the parent checkpoint as partial"
                    );
                    // A child may have committed independent evidence before
                    // timing out, but the parent must not advertise an
                    // incomplete source set. Its retry can discover and
                    // attribute the remaining children independently.
                    subagent_discovery.sources.clear();
                    subagent_discovery.incomplete = true;
                    if subagent_discovery.warning.is_none() {
                        subagent_discovery.warning = Some(
                            "bounded subagent content capture timed out; child attribution is incomplete"
                                .to_string(),
                        );
                    }
                } else {
                    return Err(error);
                }
            }
            #[cfg(test)]
            if test_support::take_fail_after_subagent_content_before_parent_checkpoint() {
                bail!("injected failure after subagent content before parent checkpoint");
            }
            // The independent child leaves have been committed above. From
            // this point forward, consume their native bytes into typed
            // snapshots so the parent checkpoint/extractor never receives a
            // raw child slice.
            let subagent_discovery_warning = subagent_discovery.warning.clone();
            if let Err(error) = write_committed_checkpoint(
                conn,
                repo,
                catalog_request.clone(),
                catalog_reservation.take().ok_or_else(|| {
                    anyhow!("capture coordinator reservation was already consumed")
                })?,
                &session_id,
                &envelope,
                binding,
                &event,
                event_id,
                identity_scheme,
                &checkpoint_id,
                capture_deadline,
                terminal_settlement_deadline,
                capture_finalizer_deadline_millis,
                &redaction_report_json,
                &all_matches,
                now,
                scope,
                preexisting_durable_terminal,
                subagent_discovery.sources,
                subagent_discovery_warning.as_deref(),
                export_lease,
            )
            .await
            {
                if !receipt_pending {
                    record_live_checkpoint_write_failure_until(
                        conn.clone(),
                        &catalog_request,
                        capture_deadline,
                    )
                    .await;
                }
                return Err(error);
            }
        }
    }

    if checkpoint != CheckpointWrite::None && repo_path.is_none() {
        // A nonterminal state transition can safely settle its receipt when
        // there is no repository checkpoint to append. A terminal transition
        // takes the other branch inside this coordinator-owned helper and
        // remains a bounded, durable finalizer pending record.
        settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation
                .take()
                .ok_or_else(|| anyhow!("capture coordinator reservation was already consumed"))?,
            &catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await?;
    }

    // The coordinator completes a pending receipt only after the checkpoint
    // store reports a durable write (or a validated durable replay). When no
    // repository path is available, terminal checkpoint-required actions
    // deliberately remain pending/replayable rather than publishing a false
    // terminal state.
    if checkpoint != CheckpointWrite::None && repo_path.is_some() {
        clear_live_retryable_checkpoint_failure_until(
            conn.clone(),
            &catalog_request,
            capture_deadline,
        )
        .await;
    }

    Ok(())
}

/// Discover the child transcripts a provider can attribute to a committed
/// parent checkpoint (ADR-ACF-10, ACF-18). A provider without a live
/// discovery capability contributes no children. A capable provider whose
/// reserved discovery slice already elapsed, or whose bounded discovery fails
/// closed, leaves the parent explicitly partial with a content-free warning.
async fn discover_subagents(
    binding: LiveCaptureBinding,
    context: &LiveCaptureContext<'_>,
    discovery_deadline: Option<Instant>,
) -> SubagentDiscovery {
    let Some(discovery) = binding.subagent_discovery() else {
        return SubagentDiscovery::default();
    };
    match discovery_deadline {
        Some(deadline) => match discovery.discover(context, deadline).await {
            Ok(discovery) => discovery,
            Err(_) => {
                tracing::warn!(
                    session_id = %redact_session_id(context.libra_session_id),
                    reason = "bounded_subagent_discovery_failed",
                    "child discovery failed closed; preserving the parent checkpoint as partial"
                );
                SubagentDiscovery {
                    warning: Some(
                        "bounded subagent content discovery failed; child attribution is incomplete"
                            .to_string(),
                    ),
                    incomplete: true,
                    ..SubagentDiscovery::default()
                }
            }
        },
        None => SubagentDiscovery {
            warning: Some(
                "subagent content skipped because the parent hook deadline has elapsed".to_string(),
            ),
            incomplete: true,
            ..SubagentDiscovery::default()
        },
    }
}

// Test-only crash knob / harness boundary: raw frames and fault injection
// below this point are compiled only for live pipeline unit tests.
// Production persistence above accepts only a validated
// `CaptureIngressCommand`.
#[cfg(test)]
#[path = "live_pipeline_tests.rs"]
mod tests;
