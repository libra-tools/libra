//! Historical external-agent transcript import orchestration (plan-20260713 M4 / DR-05).
//!
//! The command layer discovers and authorizes a source, then hands the held
//! [`TranscriptSource`] to this module. Raw provider bytes stay in memory only:
//! they are strictly parsed into typed turns, redacted field-by-field, and
//! re-serialized as an allowlist-only per-turn projection before persistence.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    authorized_read::{
        AUTHORIZED_READ_HELPER_ARG, AUTHORIZED_READ_HELPER_CAP_ENV, CancellationSafeChild,
        configure_private_helper_process_group, read_async_strictly_bounded,
        registered_helper_command,
    },
    capture::{
        catalog::{
            CaptureCatalogError, CaptureCatalogRedactionReport, CaptureCatalogSession,
            CaptureCatalogStore, CaptureImportSessionCommit, CaptureImportSessionLifecycleState,
            CaptureImportSessionPrepareRequest, CaptureImportSource,
        },
        checkpoint::{
            CheckpointRedactedPayload, CheckpointStore, CheckpointWriteOutcome,
            ImportedCheckpointWriteRequest, ImportedCheckpointWriter, TracesCheckpointStore,
        },
        snapshot::{
            CaptureSnapshot, CaptureSnapshotPartialReason, CaptureSnapshotPolicy,
            CaptureSnapshotProjection, CaptureSnapshotService,
        },
    },
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    observed_agents::{
        AgentKind, CanonValue, Completeness, ExportAuthorized, MAX_REDACTION_MATCH_SAMPLES,
        NormalizedTurn, RedactedBytes, RedactionReport, Redactor, TRANSCRIPT_READ_HARD_CAP_BYTES,
        TranscriptSource, normalize_claude_transcript_until, normalize_codex_rollout_until,
        normalize_opencode_export_until, parse_canon_value, redact_turns_with_report,
        safe_turn_projection,
    },
};
use crate::{
    internal::{
        ai::{
            coverage_gate::{self, ImportIdentityCommit, LiveClaimCommitPlan, ReservedTurnClaim},
            history::{self, TracesInflightMarker},
            hooks::{
                LifecycleEvent, LifecycleEventKind,
                lifecycle::{
                    CanonicalEventContext, LifecycleIdentityScheme,
                    lifecycle_event_canonical_json_with_identity,
                },
                runtime::build_ai_session_id,
            },
        },
        metadata::{MetadataKv, MetadataScope},
    },
    utils::util,
};

const IMPORT_IDENTITY_SCHEMA_VERSION_V1: i64 = 1;
pub(crate) const IMPORT_IDENTITY_SCHEMA_VERSION_V2: i64 = 2;
pub(crate) const IMPORT_SOURCE_PREIMAGE_BYTES: usize = 32;
/// Legacy catalog rows may retain their old repository/source fields. New
/// imports use this fixed non-correlating projection instead of a path hash.
pub(crate) const IMPORT_DURABLE_IDENTITY_NOT_RETAINED: &str = "not_retained:v1";
/// The object-index barrier is a durable import-recovery record, so its
/// source key must move with a V1→V2 identity migration rather than becoming
/// a forgotten raw-locator side channel.
pub(crate) const IMPORT_INDEX_REPAIR_MARKER_KEY: &str = "object-index-v1";
pub(crate) const IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V1: u32 = 1;
pub(crate) const IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2: u32 = 2;
const IMPORT_LEASE_MS: i64 = 60_000;
/// A failed import may make one narrowly fenced recovery attempt to release
/// its already-owned provisional rows. This is deliberately much shorter than
/// the command budget and can neither create a checkpoint nor advance an
/// import cursor.
const IMPORT_CLEANUP_GRACE: Duration = Duration::from_millis(250);
/// Import facts originate outside Libra.  Retain the same small clock-skew
/// allowance used by the traces in-flight fence, but never let a provider
/// timestamp indefinitely advance a historical session's lifecycle fence.
const IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS: i64 = 5 * 60;
const IMPORT_LIFECYCLE_EVENT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x7d, 0x9b, 0x4a, 0x51, 0x87, 0x44, 0x4d, 0x16, 0x9a, 0x6e, 0x05, 0x20, 0x26, 0x07, 0x15, 0x01,
]);

// Historical-import crash coverage needs to exercise cleanup after a durable
// attempt binding, but production hooks must never accept a process
// environment switch that changes that state machine. Keep the injection
// task-local to unit tests: integration binaries compile this crate without
// `cfg(test)` and therefore cannot arm it.
#[cfg(test)]
tokio::task_local! {
    static TEST_IMPORT_FAILPOINT: Option<&'static str>;
}

// A deterministic test-only pause after the final workspace fence and before
// a mutable import transaction commits. It proves that the post-fence
// deadline gate, rather than scheduling luck, rolls back all V2 mutations.
#[cfg(test)]
tokio::task_local! {
    static TEST_IMPORT_FINAL_FENCE_DELAY: Option<Duration>;
}

#[cfg(test)]
pub(crate) async fn with_import_failpoint<F>(name: &'static str, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_IMPORT_FAILPOINT.scope(Some(name), future).await
}

#[cfg(test)]
async fn with_import_final_fence_delay<F>(delay: Duration, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_IMPORT_FINAL_FENCE_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn import_test_delay_after_final_fence() {
    if let Ok(Some(delay)) = TEST_IMPORT_FINAL_FENCE_DELAY.try_with(|configured| *configured) {
        // This is intentionally synchronous: the transaction-owning future
        // must reach the explicit post-fence deadline check and roll back,
        // rather than merely being canceled while a database driver owns it.
        std::thread::sleep(delay);
    }
}

#[cfg(not(test))]
fn import_test_delay_after_final_fence() {}

#[derive(Clone, Copy, Debug, Error)]
pub enum ImportError {
    #[error("the transcript does not contain one unambiguous working directory")]
    WorkingDirMissingOrAmbiguous,
    #[error("the transcript belongs to a different repository")]
    RepositoryConflict,
    #[error("the provider session identity conflicts with the selected source")]
    SessionIdentityConflict,
    #[error("the selected provider session was erased locally and cannot be imported")]
    Erased,
    #[error("another import owns an unexpired lease for this provider session")]
    LeaseBusy,
    #[error("the transcript source authorization does not match this import")]
    SourceAuthorization,
    #[error("the transcript contains no importable semantic turns")]
    NoImportableTurns,
    #[error("the historical import exceeded its cumulative raw-input budget")]
    BatchInputLimit,
    #[error("the historical import exceeded its total execution deadline")]
    DeadlineExceeded,
    #[error("the authorized transcript reader is unavailable in this host")]
    AuthorizedReaderUnavailable,
    #[error("the authorized transcript reader failed without a usable result")]
    AuthorizedReaderFailed,
    #[error("the transcript contains timestamps beyond the permitted clock skew")]
    FutureTimestamp,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportRequest {
    pub agent_kind: AgentKind,
    pub provider_session_id: String,
    pub session_id: String,
    pub source_kind: String,
    pub source_id: String,
    /// V1 rows use their historical raw source locator and SHA ownership key.
    /// V2 rows use a repository-keyed source commitment and are deliberately
    /// kept in a separate uniqueness namespace.
    #[serde(default = "legacy_import_identity_schema_version")]
    pub identity_schema_version: i64,
    pub content_digest: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub session_state: String,
    pub stopped_at: Option<i64>,
    /// Verified catalog working directory. It starts as the current repository
    /// root; after a scoped existing session validation it may retain that
    /// canonical live subdirectory. Raw transcript cwd values are never
    /// persisted (ADR-DR-15 compatibility exception).
    #[serde(with = "import_path_serde")]
    pub working_dir: PathBuf,
    pub repository_identity: String,
    /// Canonical repository/worktree/workspace owner. The bounded parsing
    /// helper cannot resolve this database-backed value, so the command path
    /// attaches it before the first lease/identity write.
    #[serde(default)]
    pub capture_scope: Option<CaptureScope>,
    /// Legacy import ownership/retry key. This is intentionally separate from
    /// `CaptureSnapshotSource.identity`: callers must not treat it as safe
    /// provenance metadata or copy it into a snapshot/checkpoint projection.
    pub source_fingerprint: String,
    /// Exact non-secret fingerprint of the pre-helper existing-session row.
    /// Foreground lease acquisition re-queries and compares this value without
    /// touching the row's potentially blocking filesystem path.
    pub existing_session_fingerprint: Option<String>,
    /// Aggregate typed-field redaction evidence for the allowlisted import
    /// projection; contains counts and rule ids, never raw matched bytes.
    pub redaction_report: serde_json::Value,
    /// Safe, HMAC-bound projection created by the common capture snapshot
    /// service from the exact descriptor-pinned/export-authorized source. It
    /// may be copied into import checkpoint metadata; raw provider bytes and
    /// transient unkeyed checksums never cross that boundary.
    #[serde(default)]
    pub transcript_snapshot: Option<CaptureSnapshotProjection>,
    pub turn_boundaries: BTreeMap<String, TurnBoundary>,
    pub turns: Vec<NormalizedTurn>,
}

/// Durable recovery state for a foreground import object-index repair.
///
/// This lives with import identity rather than the command adapter because a
/// privacy migration must change its source proof atomically with both the
/// identity row and catalog metadata.  It is intentionally data-free beyond
/// the existing durable provider-session contract and an opaque V2 source
/// commitment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportIndexRepairMarker {
    pub(crate) schema_version: u32,
    pub(crate) owner: String,
    pub(crate) generation: String,
    pub(crate) identity_id: String,
    pub(crate) agent_kind: String,
    pub(crate) provider_session_id: String,
    pub(crate) source_kind: String,
    pub(crate) source_id: String,
    pub(crate) state: String,
    pub(crate) lease_expires_at: i64,
    pub(crate) created_at: i64,
    #[serde(default)]
    pub(crate) fence_token: Option<i64>,
    /// Older markers had no capture scope. They are decodable only for
    /// diagnostics; mutation paths reject them rather than guessing an owner.
    #[serde(default)]
    pub(crate) capture_scope: Option<CaptureScope>,
}

/// Validate the shape of a durable object-index recovery marker before a
/// caller makes a state-transition decision from it.  In particular, an
/// attacker/corrupt store must not make a missing identity field look like an
/// expired, safe-to-migrate V1 marker.  Scope ownership is checked by the
/// caller because the target scope is operation-specific.
pub(crate) fn validate_import_index_repair_marker(marker: &ImportIndexRepairMarker) -> Result<()> {
    if !matches!(
        marker.schema_version,
        IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V1 | IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2
    ) || marker.owner.is_empty()
        || marker.generation.is_empty()
        || marker.identity_id.is_empty()
        || AgentKind::from_db_str(&marker.agent_kind).is_none()
        || marker.provider_session_id.is_empty()
        || marker.source_kind.is_empty()
        || marker.source_id.is_empty()
        || !matches!(marker.state.as_str(), "active" | "repair_pending")
        || marker.lease_expires_at < 0
        || marker.created_at < 0
        || marker.fence_token.is_some_and(|token| token < 0)
    {
        bail!(
            "invalid durable import object-index barrier marker; run `libra agent doctor --repair`"
        );
    }
    if marker.schema_version == IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2
        && !is_import_source_commitment_v2(&marker.source_id)
    {
        bail!(
            "invalid durable import object-index source proof; run `libra agent doctor --repair`"
        );
    }
    if let Some(scope) = marker.capture_scope.as_ref()
        && (scope.repo_id.is_empty()
            || scope.workspace_id.as_deref().is_some_and(str::is_empty)
            || scope.workspace_id.is_some() != scope.workspace_fence.is_some()
            || scope.workspace_fence.is_some_and(|fence| fence < 0))
    {
        bail!(
            "invalid workspace scope on import object-index barrier marker; run `libra agent doctor --repair`"
        );
    }
    Ok(())
}

fn legacy_import_identity_schema_version() -> i64 {
    IMPORT_IDENTITY_SCHEMA_VERSION_V1
}

/// Safe, helper-returnable result of parsing one held transcript descriptor.
/// It contains only redacted semantic projection plus fixed commitments; raw
/// provider IDs, source locators, transcript working directories, and storage
/// paths remain helper-local.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedImportProjection {
    pub content_digest: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub session_state: String,
    pub stopped_at: Option<i64>,
    pub storage_commitment: [u8; IMPORT_SOURCE_PREIMAGE_BYTES],
    pub redaction_report: serde_json::Value,
    /// The helper-returned projection still contains only a transient,
    /// redacted-content SHA preimage. The command must bind it to the scoped
    /// snapshot-content HMAC before constructing an `ImportRequest`.
    pub transcript_snapshot: Option<CaptureSnapshotProjection>,
    pub turn_boundaries: BTreeMap<String, TurnBoundary>,
    pub turns: Vec<NormalizedTurn>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnBoundary {
    pub started_at: i64,
    pub ended_at: i64,
}

pub struct ImportPreparationContext<'a> {
    pub current_repo_root: &'a std::path::Path,
    pub current_storage_root: &'a std::path::Path,
    pub deadline: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingSessionOwnershipSnapshot {
    pub session_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub working_dir: String,
    pub metadata_json: String,
}

mod import_path_serde {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[cfg(unix)]
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().serialize(serializer)
    }

    #[cfg(unix)]
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        Vec::<u8>::deserialize(deserializer).map(|bytes| PathBuf::from(OsString::from_vec(bytes)))
    }

    #[cfg(not(unix))]
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        path.to_string_lossy().serialize(serializer)
    }

    #[cfg(not(unix))]
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        String::deserialize(deserializer).map(PathBuf::from)
    }
}

/// Authorized bytes already charged to the command's cumulative batch
/// budget. Fields stay private so parsing cannot be invoked on arbitrary
/// unverified provider content.
pub struct AuthorizedImportContent {
    bytes: Vec<u8>,
    provisional_session_id: String,
}

/// Counted source-read result. `raw_bytes` is meaningful even when `content`
/// is an error: malformed, unauthorized, and oversized candidates still
/// consume the shared batch budget.
pub struct ImportSourceReadOutcome {
    pub content: Result<AuthorizedImportContent>,
    pub raw_bytes: u64,
}

/// The import-specific view of the common snapshot boundary.
///
/// Historical import deliberately depends on the same service as live
/// capture, but receives only an already-authorized source. Keeping that
/// contract typed prevents a later import helper from adding a second raw
/// source read or bypassing snapshot redaction before normalization.
trait ImportSnapshotPort {
    fn capture_authorized(
        &self,
        source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot;
}

impl ImportSnapshotPort for CaptureSnapshotService {
    fn capture_authorized(
        &self,
        source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        CaptureSnapshotService::capture_authorized(source, agent_kind, libra_session_id, policy)
    }
}

/// Provider-neutral capture services consumed by historical import.
///
/// This construction point makes the source boundary explicit while keeping
/// catalog and checkpoint coordination in their own typed seams.
struct ImportCaptureServices<S> {
    snapshot: S,
}

impl ImportCaptureServices<CaptureSnapshotService> {
    fn production() -> Self {
        Self {
            snapshot: CaptureSnapshotService,
        }
    }
}

impl<S: ImportSnapshotPort> ImportCaptureServices<S> {
    fn capture_authorized(
        &self,
        source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        self.snapshot
            .capture_authorized(source, agent_kind, libra_session_id, policy)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportSummary {
    pub session_id: String,
    pub agent_kind: String,
    pub turns_seen: usize,
    pub checkpoints_written: usize,
    pub skipped_covered: usize,
    pub skipped_inflight: usize,
    pub conflicted: usize,
    pub partial: bool,
}

/// Command-only extension of the stable embedding summary. Keeping the child
/// count in this crate-private wrapper avoids changing the public struct-literal
/// and exhaustive-destructure surface of [`ImportSummary`].
#[derive(Debug, Clone)]
pub(crate) struct DetailedImportSummary {
    pub summary: ImportSummary,
    pub subagent_checkpoints_written: usize,
    /// Exact durable import identity/fence that produced this result. The
    /// command-side object-index barrier uses both values so an older process
    /// can never downgrade a newer successful writer.
    pub import_identity_id: String,
    pub import_fence_token: i64,
}

impl DetailedImportSummary {
    fn new(
        summary: ImportSummary,
        subagent_checkpoints_written: usize,
        lease: &ImportLease,
    ) -> Self {
        Self {
            summary,
            subagent_checkpoints_written,
            import_identity_id: lease.identity_id.clone(),
            import_fence_token: lease.fence_token,
        }
    }
}

#[derive(Error)]
#[error(
    "historical import has durable partial progress; run `libra agent doctor --repair` before retrying"
)]
pub struct ImportProgressError {
    pub summary: ImportSummary,
    pub(crate) subagent_checkpoints_written: usize,
    import_identity_id: String,
    import_fence_token: i64,
}

// `anyhow` callers and tracing integrations commonly render errors with
// `Debug`.  Do not let this public error turn provider/session identifiers or
// a lower-layer error chain into an accidental diagnostic sink.
impl std::fmt::Debug for ImportProgressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ImportProgressError { durable_progress: true }")
    }
}

impl ImportProgressError {
    pub(crate) fn detailed_summary(&self) -> DetailedImportSummary {
        DetailedImportSummary {
            summary: self.summary.clone(),
            subagent_checkpoints_written: self.subagent_checkpoints_written,
            import_identity_id: self.import_identity_id.clone(),
            import_fence_token: self.import_fence_token,
        }
    }
}

#[derive(Debug, Clone)]
struct ImportLease {
    identity_id: String,
    owner: String,
    fence_token: i64,
}

/// Immutable outcome details for one failed import attempt.  Grouping these
/// values keeps cleanup callers explicit without letting its internal API
/// grow a positional-argument contract each time recovery evidence evolves.
#[derive(Clone, Copy)]
struct ImportAttemptAbandonment<'a> {
    marker_fence: Option<(&'a str, &'a str)>,
    identity_state: &'a str,
    last_error_code: &'a str,
    now_ms: i64,
}

fn capture_provider_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        other => other.as_db_str(),
    }
}

fn timestamp_seconds(value: &CanonValue) -> Option<i64> {
    match value {
        CanonValue::Int(value) if *value > 10_000_000_000 => Some(*value / 1_000),
        CanonValue::Int(value) => Some(*value),
        CanonValue::Str(value) => DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|value| value.timestamp()),
        _ => None,
    }
}

fn collect_timestamp_fields(value: &CanonValue, facts: &mut SourceFacts) {
    for field in ["created", "updated"] {
        if let Some(timestamp) = value.get(field).and_then(timestamp_seconds) {
            facts.timestamps.push(timestamp);
        }
    }
}

fn ensure_before_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(ImportError::DeadlineExceeded.into());
    }
    Ok(())
}

/// Acquire the SQLite writer slot only while the invocation's monotonic half
/// is still live. Once the slot is held, final authorization below evaluates
/// the immutable SQLite half immediately before the non-cancellable COMMIT.
async fn begin_import_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<sea_orm::DatabaseTransaction> {
    ensure_before_deadline(deadline.monotonic())?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(ImportError::DeadlineExceeded))?
    .with_context(|| format!("{operation}: acquire import database writer"))
}

/// Bound only a read-only preflight in a normal import invocation. DML,
/// final authorization, rollback, and COMMIT acknowledgement deliberately
/// remain outside this helper: cancelling one of those futures can leave the
/// durable import outcome unknowable.
async fn await_import_precommit_read_until<T>(
    deadline: Instant,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_before_deadline(deadline)?;
    let value = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), read)
        .await
        .map_err(|_| anyhow::Error::from(ImportError::DeadlineExceeded))??;
    ensure_before_deadline(deadline)?;
    Ok(value)
}

/// Cleanup is the narrow exception to an import's primary deadline: it can
/// release only the caller's existing fenced reservations. Bound its writer
/// acquisition and pre-commit work to a short recovery grace, then retain the
/// paired final authorization and non-cancellable COMMIT acknowledgement.
async fn begin_import_cleanup_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<sea_orm::DatabaseTransaction> {
    ensure_before_deadline(deadline.monotonic())?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(ImportError::DeadlineExceeded))?
    .with_context(|| format!("{operation}: acquire import cleanup database writer"))
}

fn import_cleanup_deadline(deadline: CaptureCommitDeadline) -> Result<CaptureCommitDeadline> {
    let recovery_deadline = CaptureCommitDeadline::from_budget(IMPORT_CLEANUP_GRACE)
        .map_err(|_| anyhow::Error::from(ImportError::DeadlineExceeded))?;
    let now = Instant::now();
    // A live primary deadline remains a ceiling in both time domains. In
    // particular, a long monotonic half must not re-anchor an already-expired
    // SQLite authorization deadline to fresh cleanup grace.
    Ok(match (deadline.monotonic() > now).then_some(deadline) {
        Some(deadline) => CaptureCommitDeadline::from_established_pair(
            deadline.monotonic().min(recovery_deadline.monotonic()),
            deadline
                .sqlite_not_after_millis()
                .min(recovery_deadline.sqlite_not_after_millis()),
        ),
        None => recovery_deadline,
    })
}

/// Make the final database authorization the transaction's last SQL
/// operation, then await COMMIT without a cancellation timeout. SQLx may
/// dispatch COMMIT before an interrupted acknowledgement future is dropped;
/// only the SQLite-side authorization can prove an expired invocation did not
/// publish a mutable import record.
async fn commit_import_transaction_until(
    txn: sea_orm::DatabaseTransaction,
    scope: &CaptureScope,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<()> {
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error);
    }
    import_test_delay_after_final_fence();
    let authorization = authorize_final_capture_commit(Some(scope), &txn, Some(deadline)).await;
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(match error {
            CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                anyhow::Error::from(ImportError::DeadlineExceeded)
            }
            error @ (CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
            | CaptureFinalCommitAuthorizationError::Database(_)) => anyhow::Error::new(error),
        })
        .with_context(|| format!("{operation}: authorize final import database commit"));
    }
    txn.commit().await.with_context(|| operation)
}

#[derive(Default)]
struct SourceFacts {
    working_dirs: BTreeSet<PathBuf>,
    timestamps: Vec<i64>,
    provider_session_ids: BTreeSet<String>,
    terminal: bool,
}

fn import_timestamp_limit(trusted_now: i64) -> i64 {
    trusted_now.saturating_add(IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS)
}

fn validate_import_timestamp(timestamp: i64, trusted_now: i64) -> Result<()> {
    if timestamp > import_timestamp_limit(trusted_now) {
        return Err(ImportError::FutureTimestamp.into());
    }
    Ok(())
}

/// Validate every provider-owned time before it can influence chronology.
/// Provider metadata and normalized turns are independently parsed inputs, so
/// both surfaces must be checked before the monotonic-boundary synthesis
/// below has a chance to carry a malicious future value into durable state.
fn validate_untrusted_import_timestamps(
    facts: &SourceFacts,
    turns: &[NormalizedTurn],
    trusted_now: i64,
) -> Result<()> {
    for timestamp in &facts.timestamps {
        validate_import_timestamp(*timestamp, trusted_now)?;
    }
    for turn in turns {
        if let Some(timestamp) = turn.started_at {
            validate_import_timestamp(timestamp, trusted_now)?;
        }
        if let Some(timestamp) = turn.ended_at {
            validate_import_timestamp(timestamp, trusted_now)?;
        }
    }
    Ok(())
}

/// Derive a monotonic historical chronology only after every untrusted source
/// fact has passed the future-skew gate.  Validate the derived values again:
/// a transcript with many omitted timestamps can otherwise synthesize a
/// future end time simply through its ordinal fallback.
fn build_import_chronology(
    facts: &SourceFacts,
    turns: &[NormalizedTurn],
    trusted_now: i64,
) -> Result<(BTreeMap<String, TurnBoundary>, i64, i64)> {
    validate_untrusted_import_timestamps(facts, turns, trusted_now)?;
    let source_started_at = facts
        .timestamps
        .iter()
        .copied()
        .min()
        .unwrap_or(trusted_now);
    let source_ended_at = facts
        .timestamps
        .iter()
        .copied()
        .max()
        .unwrap_or(source_started_at);
    let mut turn_boundaries = BTreeMap::new();
    let mut previous_ended_at: Option<i64> = None;
    for turn in turns {
        let ordinal = i64::try_from(turn.ordinal).context("turn ordinal exceeds time range")?;
        let fallback = source_started_at
            .checked_add(ordinal)
            .context("turn chronology exceeds timestamp range")?;
        let raw_started_at = turn.started_at.unwrap_or(fallback);
        let turn_started_at = match previous_ended_at {
            Some(previous) => raw_started_at.max(
                previous
                    .checked_add(1)
                    .context("turn chronology exceeds timestamp range")?,
            ),
            None => raw_started_at,
        };
        let raw_ended_at = turn.ended_at.unwrap_or(turn_started_at);
        let turn_ended_at = raw_ended_at.max(turn_started_at);
        validate_import_timestamp(turn_started_at, trusted_now)?;
        validate_import_timestamp(turn_ended_at, trusted_now)?;
        previous_ended_at = Some(turn_ended_at);
        turn_boundaries.insert(
            turn.logical_turn_key.clone(),
            TurnBoundary {
                started_at: turn_started_at,
                ended_at: turn_ended_at,
            },
        );
    }
    let started_at = turn_boundaries
        .get(
            &turns
                .first()
                .context("normalized import has no first turn")?
                .logical_turn_key,
        )
        .map(|boundary| boundary.started_at.min(source_started_at))
        .context("normalized import has no first-turn chronology")?;
    let ended_at = previous_ended_at
        .context("normalized import has no final-turn chronology")?
        .max(source_ended_at);
    validate_import_timestamp(started_at, trusted_now)?;
    validate_import_timestamp(ended_at, trusted_now)?;
    Ok((turn_boundaries, started_at, ended_at))
}

/// Defense in depth for direct library callers and the parent side of the
/// preparation-helper protocol.  `prepare_import_request` validates source
/// facts before they are discarded; this second gate validates every field
/// that can be persisted before an identity lease or database write begins.
fn validate_persisted_import_timestamps(request: &ImportRequest, trusted_now: i64) -> Result<()> {
    validate_import_timestamp(request.started_at, trusted_now)?;
    validate_import_timestamp(request.ended_at, trusted_now)?;
    if let Some(stopped_at) = request.stopped_at {
        validate_import_timestamp(stopped_at, trusted_now)?;
    }
    for boundary in request.turn_boundaries.values() {
        validate_import_timestamp(boundary.started_at, trusted_now)?;
        validate_import_timestamp(boundary.ended_at, trusted_now)?;
    }
    // `turns` are later converted into a durable, typed checkpoint
    // projection. Validate their original optional times as well, even when a
    // malformed request omitted the corresponding boundary map entry.
    for turn in &request.turns {
        if let Some(timestamp) = turn.started_at {
            validate_import_timestamp(timestamp, trusted_now)?;
        }
        if let Some(timestamp) = turn.ended_at {
            validate_import_timestamp(timestamp, trusted_now)?;
        }
    }
    Ok(())
}

fn collect_facts(kind: AgentKind, bytes: &[u8], deadline: Option<Instant>) -> Result<SourceFacts> {
    let mut facts = SourceFacts::default();
    match kind {
        AgentKind::ClaudeCode | AgentKind::Codex => {
            for line in bytes.split(|byte| *byte == b'\n') {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(ImportError::DeadlineExceeded.into());
                }
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let Ok(entry) = parse_canon_value(line) else {
                    // The coverage normalizer marks this turn incomplete.
                    // Metadata discovery still uses only independently valid,
                    // duplicate-key-free records, so a truncated tail can be
                    // imported and later upgraded without trusting malformed
                    // cwd/session fields.
                    continue;
                };
                if let Some(timestamp) = entry.get("timestamp").and_then(timestamp_seconds) {
                    facts.timestamps.push(timestamp);
                }
                let entry_type = entry.get("type").and_then(CanonValue::as_str);
                if matches!(entry_type, Some("session_end" | "session-ended")) {
                    facts.terminal = true;
                } else if matches!(entry_type, Some("user" | "assistant" | "response_item")) {
                    // A provider may append a resumed session to the same
                    // JSONL after an earlier terminal record. Later semantic
                    // activity reopens it until a newer terminal record is
                    // observed.
                    facts.terminal = false;
                }
                if kind == AgentKind::ClaudeCode {
                    if let Some(cwd) = entry.get("cwd").and_then(CanonValue::as_str) {
                        facts.working_dirs.insert(PathBuf::from(cwd));
                    }
                    if let Some(id) = entry
                        .get("sessionId")
                        .or_else(|| entry.get("session_id"))
                        .and_then(CanonValue::as_str)
                    {
                        facts.provider_session_ids.insert(id.to_string());
                    }
                } else if let Some(payload) = entry.get("payload") {
                    let payload_type = payload.get("type").and_then(CanonValue::as_str);
                    if matches!(payload_type, Some("session_end" | "session-ended")) {
                        facts.terminal = true;
                    } else if matches!(entry_type, Some("response_item" | "event_msg"))
                        && matches!(
                            payload_type,
                            Some(
                                "message"
                                    | "function_call"
                                    | "custom_tool_call"
                                    | "function_call_output"
                                    | "custom_tool_call_output"
                                    | "task_started"
                                    | "turn_started"
                            )
                        )
                    {
                        facts.terminal = false;
                    }
                    if let Some(cwd) = payload.get("cwd").and_then(CanonValue::as_str) {
                        facts.working_dirs.insert(PathBuf::from(cwd));
                    }
                    if entry.get("type").and_then(CanonValue::as_str) == Some("session_meta")
                        && let Some(id) = payload.get("id").and_then(CanonValue::as_str)
                    {
                        facts.provider_session_ids.insert(id.to_string());
                    }
                }
            }
        }
        AgentKind::OpenCode => {
            let document = parse_canon_value(bytes)
                .context("parse OpenCode export metadata with duplicate-key rejection")?;
            if let Some(info) = document.get("info") {
                if matches!(
                    info.get("status").and_then(CanonValue::as_str),
                    Some("idle" | "completed" | "stopped")
                ) {
                    facts.terminal = true;
                }
                if let Some(cwd) = info
                    .get("directory")
                    .or_else(|| info.get("cwd"))
                    .and_then(CanonValue::as_str)
                {
                    facts.working_dirs.insert(PathBuf::from(cwd));
                }
                if let Some(id) = info.get("id").and_then(CanonValue::as_str) {
                    facts.provider_session_ids.insert(id.to_string());
                }
                collect_timestamp_fields(info, &mut facts);
                if let Some(time) = info.get("time") {
                    collect_timestamp_fields(time, &mut facts);
                }
            }
            if let Some(messages) = document.get("messages").and_then(CanonValue::as_array) {
                for message in messages {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(ImportError::DeadlineExceeded.into());
                    }
                    if let Some(info) = message.get("info") {
                        collect_timestamp_fields(info, &mut facts);
                        if let Some(time) = info.get("time") {
                            collect_timestamp_fields(time, &mut facts);
                        }
                    }
                }
            }
        }
        _ => bail!("agent kind '{}' is not importable", kind.as_cli_slug()),
    }
    Ok(facts)
}

/// Derive the unique provider session id from an authorized source after the
/// caller has obtained import consent. The file descriptor is rewound, so the
/// real import consumes the same pinned source rather than reopening a path.
pub fn provider_session_id_from_source(
    kind: AgentKind,
    source: &mut TranscriptSource,
) -> Result<String> {
    let preview = match source {
        TranscriptSource::File { file, .. } => {
            file.preview_bounded(TRANSCRIPT_READ_HARD_CAP_BYTES)?
        }
        TranscriptSource::Bytes { bytes, .. } => bytes.clone(),
    };
    let facts = collect_facts(kind, &preview, None)?;
    if facts.provider_session_ids.len() != 1 {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    facts
        .provider_session_ids
        .into_iter()
        .next()
        .ok_or_else(|| ImportError::SessionIdentityConflict.into())
}

fn verified_repo_root(
    facts: &SourceFacts,
    current_repo_root: &std::path::Path,
    current_storage_root: &std::path::Path,
) -> Result<PathBuf> {
    // agent_session.working_dir is a stable SQLite TEXT contract consumed by
    // older releases. Reject a non-UTF-8 repository explicitly instead of
    // lossy-converting it during persistence and making replay unverifiable.
    if current_repo_root.to_str().is_none() || current_storage_root.to_str().is_none() {
        return Err(ImportError::WorkingDirMissingOrAmbiguous.into());
    }
    let current_repo_root = current_repo_root
        .canonicalize()
        .context("canonicalize current repository root")?;
    let current_storage_root = current_storage_root
        .canonicalize()
        .context("canonicalize current repository storage")?;
    let mut resolved = BTreeSet::new();
    for cwd in &facts.working_dirs {
        let canonical = cwd
            .canonicalize()
            .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?;
        resolved.insert(canonical);
    }
    if resolved.len() != 1 {
        return Err(ImportError::WorkingDirMissingOrAmbiguous.into());
    }
    let cwd = resolved
        .first()
        .ok_or(ImportError::WorkingDirMissingOrAmbiguous)?;
    let source_storage = util::try_get_storage_path(Some(cwd.clone()))
        .map_err(|_| ImportError::RepositoryConflict)?
        .canonicalize()
        .map_err(|_| ImportError::RepositoryConflict)?;
    // Linked worktrees have sibling worktree roots but intentionally share
    // one canonical Libra storage directory. The shared storage identity is
    // the repository boundary; requiring the transcript cwd to be below the
    // currently checked-out worktree would reject a valid sibling worktree.
    if source_storage != current_storage_root {
        return Err(ImportError::RepositoryConflict.into());
    }
    Ok(current_repo_root)
}

fn source_digest(turns: &[NormalizedTurn]) -> String {
    let mut digest = Sha256::new();
    for turn in turns {
        digest.update(turn.logical_turn_key.as_bytes());
        digest.update([0]);
        digest.update(turn.digest_hex().as_bytes());
        digest.update([0xff]);
    }
    hex::encode(digest.finalize())
}

fn update_length_delimited_import_preimage(digest: &mut Sha256, value: &[u8]) -> Result<()> {
    let length = u64::try_from(value.len()).context("import identity field length exceeds u64")?;
    digest.update(length.to_be_bytes());
    digest.update(value);
    Ok(())
}

/// A fixed, transient commitment for matching a selected provider session to
/// facts parsed by the descriptor-owning helper. It is neither durable nor a
/// source identity: using a separate domain prevents a future caller from
/// substituting it for the repository-keyed V2 source commitment.
pub(crate) fn import_provider_commitment(provider_session_id: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"libra-agent-import-provider-commitment-v2\0");
    // This fixed string is valid UTF-8 and bounded by the command parser;
    // hashing cannot fail and keeps this helper usable at a low-level IPC
    // boundary where a fallible allocation would add no safety value.
    digest.update((provider_session_id.len() as u64).to_be_bytes());
    digest.update(provider_session_id.as_bytes());
    digest.finalize().into()
}

/// Construct the transient source preimage from the parent-held authorized
/// source identity. It is never serialized into metadata or helper control;
/// only the later repository-keyed HMAC may become durable.
pub(crate) fn import_source_preimage(
    agent_kind: AgentKind,
    source_kind: &str,
    source_id: &str,
    provider_session_id: &str,
) -> Result<[u8; IMPORT_SOURCE_PREIMAGE_BYTES]> {
    let mut digest = Sha256::new();
    digest.update(b"libra-agent-import-source-preimage-v2\0");
    for value in [
        agent_kind.as_db_str().as_bytes(),
        source_kind.as_bytes(),
        source_id.as_bytes(),
        provider_session_id.as_bytes(),
    ] {
        update_length_delimited_import_preimage(&mut digest, value)?;
    }
    Ok(digest.finalize().into())
}

/// Commit a canonical Libra storage root without putting a transcript working
/// directory or storage path on the private helper response wire. Linked
/// worktrees and repository subdirectories intentionally share this identity.
pub(crate) fn import_storage_commitment(storage_root: &Path) -> Result<[u8; 32]> {
    let canonical = storage_root
        .canonicalize()
        .map_err(|_| ImportError::RepositoryConflict)?;
    let path = canonical.to_str().ok_or(ImportError::RepositoryConflict)?;
    let mut digest = Sha256::new();
    digest.update(b"libra-agent-import-storage-commitment-v2\0");
    update_length_delimited_import_preimage(&mut digest, path.as_bytes())?;
    Ok(digest.finalize().into())
}

/// Preserve the existing compact import report shape while accounting for
/// both redaction passes that now protect an import: the common snapshot
/// service scrubs the bounded provider source before normalization, then the
/// typed allowlist pass protects normalized semantic fields.  Reports are
/// merged under the global sample cap so a source with many secrets cannot
/// grow a durable session/checkpoint record without bound.
fn merged_import_redaction_report(
    snapshot: &RedactionReport,
    typed: &RedactionReport,
) -> serde_json::Value {
    let mut matches = Vec::with_capacity(
        snapshot
            .matches
            .len()
            .saturating_add(typed.matches.len())
            .min(MAX_REDACTION_MATCH_SAMPLES),
    );
    let mut dropped_matches = snapshot
        .dropped_matches
        .saturating_add(typed.dropped_matches);
    for matched in snapshot.matches.iter().chain(typed.matches.iter()) {
        if matches.len() < MAX_REDACTION_MATCH_SAMPLES {
            matches.push(matched.clone());
        } else {
            dropped_matches = dropped_matches.saturating_add(1);
        }
    }
    let mut report = serde_json::json!({
        // Keep the established name for downstream readers. The additional
        // boolean is additive evidence that source-level redaction happened
        // before any importer normalization or persistence projection.
        "pipeline": "typed_allowlist",
        "snapshot_redaction": true,
        "raw_persisted": false,
        "matches": matches,
        "bytes_scanned": snapshot.bytes_scanned.saturating_add(typed.bytes_scanned),
        "bytes_redacted": snapshot.bytes_redacted.saturating_add(typed.bytes_redacted),
    });
    if dropped_matches != 0
        && let Some(object) = report.as_object_mut()
    {
        object.insert(
            "dropped_matches".to_string(),
            serde_json::Value::from(dropped_matches),
        );
    }
    report
}

fn snapshot_failure_for_import(snapshot: &CaptureSnapshotProjection) -> ImportError {
    match snapshot.partial_reason {
        Some(CaptureSnapshotPartialReason::DeadlineExceeded) => ImportError::DeadlineExceeded,
        Some(CaptureSnapshotPartialReason::SourceOversize) => ImportError::BatchInputLimit,
        // The source has already crossed the explicit consent, pinning, and
        // authorization boundary. Any remaining partial reason is safest to
        // surface through the established source-authorization failure path,
        // which is intentionally content-free to the caller.
        Some(
            CaptureSnapshotPartialReason::SourceAbsent
            | CaptureSnapshotPartialReason::SourceUntrusted
            | CaptureSnapshotPartialReason::SourceAuthorizationMismatch
            | CaptureSnapshotPartialReason::SourceCommitmentUnavailable
            | CaptureSnapshotPartialReason::SourceReadError
            | CaptureSnapshotPartialReason::SourceEmpty,
        )
        | None => ImportError::SourceAuthorization,
    }
}

/// Read one already-authorized descriptor through the private Libra helper.
///
/// The descriptor is deliberately attached directly to helper stdin rather
/// than copied into a parent-owned pipe.  That preserves the one-open-handle
/// authorization property while letting a stalled filesystem read be killed,
/// bounded, and reaped under the import deadline.  The helper's output is a
/// tiny fixed header followed by at most `read_cap` bytes; stderr is discarded
/// because it can otherwise carry provider filesystem details.
async fn read_authorized_descriptor_until(
    file: std::fs::File,
    read_cap: u64,
    deadline: Instant,
) -> (Result<Vec<u8>>, u64) {
    #[cfg(not(unix))]
    {
        let _ = (file, read_cap, deadline);
        // The raw descriptor reader relies on Unix process-group containment
        // for forked helper descendants. Do not spawn it where that contract
        // is unavailable.
        return (Err(ImportError::AuthorizedReaderUnavailable.into()), 0);
    }

    #[cfg(unix)]
    {
        if let Err(error) = ensure_before_deadline(deadline) {
            return (Err(error), 0);
        }
        let Some(mut command) = registered_helper_command(AUTHORIZED_READ_HELPER_ARG) else {
            // Library embedding is intentionally fail-closed: an unrelated host
            // must never be invoked with Libra's private helper argument.
            return (Err(ImportError::AuthorizedReaderUnavailable.into()), 0);
        };
        // The response is status + raw byte count + at most one cap-sized
        // payload. The shared reader probes a one-byte overflow on the stack.
        let output_cap = read_cap.saturating_add(9);
        command
            .env_clear()
            .current_dir(std::path::Path::new("/"))
            .env(AUTHORIZED_READ_HELPER_CAP_ENV, read_cap.to_string())
            .stdin(Stdio::from(file))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return (Err(ImportError::AuthorizedReaderFailed.into()), 0),
        };
        let mut child = CancellationSafeChild::new_process_group(child);
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            let _ = child.terminate_and_reap_checked();
            return (Err(ImportError::AuthorizedReaderFailed.into()), 0);
        };
        let mut stdout_task =
            tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
        child.register_abort_on_cancel(&stdout_task);

        // Drain stdout before reaping the direct leader. A helper can fork/exec a
        // descendant retaining both the raw descriptor stdin and stdout; keeping
        // the leader unreaped lets deadline/Drop kill its dedicated process group
        // before that PGID could ever be recycled.
        let output = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut stdout_task,
        )
        .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                let _ = child.terminate_and_reap_checked();
                return (Err(ImportError::AuthorizedReaderFailed.into()), 0);
            }
            Err(_) => {
                let error = if child.terminate_and_reap_checked().is_ok() {
                    ImportError::DeadlineExceeded
                } else {
                    ImportError::AuthorizedReaderFailed
                };
                return (Err(error.into()), 0);
            }
        };
        let status = match child.child_mut() {
            Some(child_process) => match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(_)) => {
                    let _ = child.terminate_and_reap_checked();
                    return (Err(ImportError::AuthorizedReaderFailed.into()), 0);
                }
                Err(_) => {
                    let error = if child.terminate_and_reap_checked().is_ok() {
                        ImportError::DeadlineExceeded
                    } else {
                        ImportError::AuthorizedReaderFailed
                    };
                    return (Err(error.into()), 0);
                }
            },
            None => return (Err(ImportError::AuthorizedReaderFailed.into()), 0),
        };
        // stdout reached EOF before the direct leader was reaped, so this is the
        // only safe point to disarm its process-group identifier.
        child.disarm_child_after_wait();
        child.finish();
        if Instant::now() >= deadline {
            return (Err(ImportError::DeadlineExceeded.into()), 0);
        }
        if !status.success() {
            return (Err(ImportError::AuthorizedReaderFailed.into()), 0);
        }
        decode_authorized_descriptor_frame(&output, read_cap)
    }
}

/// Decode the private held-descriptor reader frame without ever reflecting a
/// helper-provided error string into the caller.  Status 2 is intentionally
/// data-free; accepting a payload there would recreate a raw filesystem-error
/// side channel if a future helper implementation changed its behavior.
fn decode_authorized_descriptor_frame(frame: &[u8], read_cap: u64) -> (Result<Vec<u8>>, u64) {
    let fail = || (Err(ImportError::AuthorizedReaderFailed.into()), 0);
    if frame.len() < 9 {
        return fail();
    }
    let Ok(frame_len) = u64::try_from(frame.len()) else {
        return fail();
    };
    if frame_len > read_cap.saturating_add(9) {
        return fail();
    }
    let Ok(raw_header) = <[u8; 8]>::try_from(&frame[1..9]) else {
        return fail();
    };
    let raw_bytes = u64::from_le_bytes(raw_header);
    let payload = &frame[9..];
    let payload_len = match u64::try_from(payload.len()) {
        Ok(length) => length,
        Err(_) => return fail(),
    };
    match frame[0] {
        0 if raw_bytes <= read_cap && payload_len == raw_bytes => (Ok(payload.to_vec()), raw_bytes),
        1 if raw_bytes == read_cap.saturating_add(1) && payload.is_empty() => (
            Err(
                super::observed_agents::transcript_source::TranscriptReadError::ExceedsCap {
                    cap: read_cap,
                }
                .into(),
            ),
            raw_bytes,
        ),
        // Status 2 is a fixed, payload-free reader failure.  It can report
        // bytes already pulled from the descriptor so the shared batch budget
        // still charges an interrupted read, but it never transports an OS
        // error or provider locator to this process.
        2 if raw_bytes <= read_cap.saturating_add(1) && payload.is_empty() => {
            (Err(ImportError::AuthorizedReaderFailed.into()), raw_bytes)
        }
        _ => fail(),
    }
}

/// Consume an authorized source exactly once and report the bytes pulled from
/// it independently of validation success.
pub async fn read_import_source(
    agent_kind: AgentKind,
    selected_provider_session_id: &str,
    source: TranscriptSource,
    remaining_raw_bytes: u64,
    deadline: Instant,
) -> ImportSourceReadOutcome {
    if let Err(error) = ensure_before_deadline(deadline) {
        return ImportSourceReadOutcome {
            content: Err(error),
            raw_bytes: 0,
        };
    }
    let provisional_session_id = build_ai_session_id(
        capture_provider_name(agent_kind),
        selected_provider_session_id,
    );
    let read_cap = remaining_raw_bytes.min(TRANSCRIPT_READ_HARD_CAP_BYTES);
    match source {
        TranscriptSource::File { file, .. } => {
            let file = match file.into_rewound_inner() {
                Ok(file) => file,
                Err(_) => {
                    return ImportSourceReadOutcome {
                        content: Err(ImportError::AuthorizedReaderFailed.into()),
                        raw_bytes: 0,
                    };
                }
            };
            let (read, raw_bytes) =
                read_authorized_descriptor_until(file, read_cap, deadline).await;
            let content = match read {
                Ok(bytes) => Ok(AuthorizedImportContent {
                    bytes,
                    provisional_session_id,
                }),
                Err(error)
                    if error
                        .downcast_ref::<
                            super::observed_agents::transcript_source::TranscriptReadError,
                        >()
                        .is_some() =>
                {
                    Err(ImportError::BatchInputLimit.into())
                }
                Err(error) => Err(error),
            };
            ImportSourceReadOutcome { content, raw_bytes }
        }
        TranscriptSource::Bytes { bytes, auth } => {
            let raw_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let content = if !auth.matches(agent_kind.as_db_str(), &provisional_session_id, &bytes)
            {
                Err(ImportError::SourceAuthorization.into())
            } else if raw_bytes > read_cap {
                Err(ImportError::BatchInputLimit.into())
            } else {
                Ok(AuthorizedImportContent {
                    bytes,
                    provisional_session_id,
                })
            };
            ImportSourceReadOutcome { content, raw_bytes }
        }
    }
}

/// Parse authorized, already-budgeted bytes and build the transient import
/// request. The returned request contains no raw provider payload or path.
pub fn prepare_import_request(
    agent_kind: AgentKind,
    selected_provider_session_id: &str,
    source_kind: &str,
    source_id: &str,
    content: AuthorizedImportContent,
    context: ImportPreparationContext<'_>,
) -> Result<ImportRequest> {
    let ImportPreparationContext {
        current_repo_root,
        current_storage_root,
        deadline,
    } = context;
    // Capture one trusted wall-clock reference for this whole preparation.
    // Every provider fact and every synthesized persistence timestamp is
    // compared to this same value before the request can reach a lease.
    let trusted_now = Utc::now().timestamp();
    ensure_before_deadline(deadline)?;
    let AuthorizedImportContent {
        bytes,
        provisional_session_id,
    } = content;
    ensure_before_deadline(deadline)?;
    let facts = collect_facts(agent_kind, &bytes, Some(deadline))?;
    for timestamp in &facts.timestamps {
        validate_import_timestamp(*timestamp, trusted_now)?;
    }
    ensure_before_deadline(deadline)?;
    if facts.provider_session_ids.len() != 1
        || !facts
            .provider_session_ids
            .contains(selected_provider_session_id)
    {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    let working_dir = verified_repo_root(&facts, current_repo_root, current_storage_root)?;
    // The raw vector has served its only import-specific purpose: validating
    // provider identity and the repository boundary inside this killable
    // preparation helper. Hand ownership to the shared snapshot service now;
    // downstream normalization, projection, metadata, and checkpoint writes
    // can observe only redacted bytes plus safe provenance.
    let capture_services = ImportCaptureServices::production();
    let snapshot = capture_services.capture_authorized(
        TranscriptSource::Bytes {
            auth: ExportAuthorized::issue(agent_kind.as_db_str(), &provisional_session_id, &bytes),
            bytes,
        },
        agent_kind.as_db_str(),
        &provisional_session_id,
        CaptureSnapshotPolicy::with_deadline(Some(deadline)),
    );
    let transcript_snapshot = snapshot.safe_projection();
    let snapshot_redaction_report = snapshot.redaction_report().clone();
    let transcript = snapshot
        .into_redacted_transcript()
        .ok_or_else(|| snapshot_failure_for_import(&transcript_snapshot))?;
    ensure_before_deadline(deadline)?;
    let mut turns = match agent_kind {
        AgentKind::ClaudeCode => normalize_claude_transcript_until(transcript.bytes(), deadline),
        AgentKind::Codex => normalize_codex_rollout_until(transcript.bytes(), deadline),
        AgentKind::OpenCode => normalize_opencode_export_until(transcript.bytes(), deadline),
        _ => bail!(
            "agent kind '{}' is not importable",
            agent_kind.as_cli_slug()
        ),
    }
    .ok_or(ImportError::DeadlineExceeded)?;
    ensure_before_deadline(deadline)?;
    let typed_redaction_report = redact_turns_with_report(&mut turns);
    ensure_before_deadline(deadline)?;
    if turns.is_empty() {
        return Err(ImportError::NoImportableTurns.into());
    }
    // A transcript without explicit terminal evidence may still be growing.
    // Its final turn remains upgradeable even when the current JSONL tail is
    // syntactically complete.
    if !facts.terminal
        && let Some(last) = turns.last_mut()
    {
        last.completeness = Completeness::Incomplete;
    }
    let (turn_boundaries, started_at, ended_at) =
        build_import_chronology(&facts, &turns, trusted_now)?;
    let canonical_storage = current_storage_root
        .canonicalize()
        .context("canonicalize verified import repository storage")?;
    let repository_identity = hex::encode(Sha256::digest(
        canonical_storage.to_string_lossy().as_bytes(),
    ));
    let source_fingerprint = hex::encode(Sha256::digest(
        format!("{}\0{}\0{}", agent_kind.as_db_str(), source_kind, source_id).as_bytes(),
    ));
    let redaction_report =
        merged_import_redaction_report(&snapshot_redaction_report, &typed_redaction_report);
    Ok(ImportRequest {
        agent_kind,
        provider_session_id: selected_provider_session_id.to_string(),
        session_id: provisional_session_id,
        source_kind: source_kind.to_string(),
        source_id: source_id.to_string(),
        identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
        content_digest: source_digest(&turns),
        started_at,
        ended_at,
        session_state: if facts.terminal {
            "stopped".to_string()
        } else {
            "active".to_string()
        },
        stopped_at: facts.terminal.then_some(ended_at),
        working_dir,
        repository_identity,
        capture_scope: None,
        source_fingerprint,
        existing_session_fingerprint: None,
        redaction_report,
        transcript_snapshot: Some(transcript_snapshot),
        turn_boundaries,
        turns,
    })
}

/// Parse one held descriptor in the private preparation helper without
/// returning any raw provider identity, locator, or working-directory path.
/// The command validates the two fixed commitments and reconstructs the
/// compatibility session fields only after its scope/root checks succeed.
pub(crate) fn prepare_import_projection(
    agent_kind: AgentKind,
    expected_provider_commitment: [u8; 32],
    bytes: Vec<u8>,
    deadline: Instant,
) -> Result<PreparedImportProjection> {
    let trusted_now = Utc::now().timestamp();
    ensure_before_deadline(deadline)?;
    let facts = collect_facts(agent_kind, &bytes, Some(deadline))?;
    for timestamp in &facts.timestamps {
        validate_import_timestamp(*timestamp, trusted_now)?;
    }
    ensure_before_deadline(deadline)?;
    if facts.provider_session_ids.len() != 1 {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    let provider_session_id = facts
        .provider_session_ids
        .first()
        .ok_or(ImportError::SessionIdentityConflict)?;
    if import_provider_commitment(provider_session_id) != expected_provider_commitment {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    // Transcript records can spell one directory through equivalent lexical
    // paths (`/repo` and `/repo/.`, or a symlink alias).  Deduplicate the
    // canonical paths rather than the untrusted spellings so one real
    // working directory is not rejected as ambiguous.
    let mut resolved_working_dirs = BTreeSet::new();
    for working_dir in &facts.working_dirs {
        ensure_before_deadline(deadline)?;
        let canonical = working_dir
            .canonicalize()
            .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?;
        resolved_working_dirs.insert(canonical);
    }
    ensure_before_deadline(deadline)?;
    let working_dir = if resolved_working_dirs.len() == 1 {
        resolved_working_dirs
            .into_iter()
            .next()
            .ok_or(ImportError::WorkingDirMissingOrAmbiguous)?
    } else {
        return Err(ImportError::WorkingDirMissingOrAmbiguous.into());
    };
    let storage_root = util::try_get_storage_path(Some(working_dir))
        .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?
        .canonicalize()
        .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?;
    let storage_commitment = import_storage_commitment(&storage_root)?;
    let provisional_session_id =
        build_ai_session_id(capture_provider_name(agent_kind), provider_session_id);
    let capture_services = ImportCaptureServices::production();
    let snapshot = capture_services.capture_authorized(
        TranscriptSource::Bytes {
            auth: ExportAuthorized::issue(agent_kind.as_db_str(), &provisional_session_id, &bytes),
            bytes,
        },
        agent_kind.as_db_str(),
        &provisional_session_id,
        CaptureSnapshotPolicy::with_deadline(Some(deadline)),
    );
    let transcript_snapshot = snapshot.safe_projection();
    let snapshot_redaction_report = snapshot.redaction_report().clone();
    let transcript = snapshot
        .into_redacted_transcript()
        .ok_or_else(|| snapshot_failure_for_import(&transcript_snapshot))?;
    ensure_before_deadline(deadline)?;
    let mut turns = match agent_kind {
        AgentKind::ClaudeCode => normalize_claude_transcript_until(transcript.bytes(), deadline),
        AgentKind::Codex => normalize_codex_rollout_until(transcript.bytes(), deadline),
        AgentKind::OpenCode => normalize_opencode_export_until(transcript.bytes(), deadline),
        _ => bail!(
            "agent kind '{}' is not importable",
            agent_kind.as_cli_slug()
        ),
    }
    .ok_or(ImportError::DeadlineExceeded)?;
    ensure_before_deadline(deadline)?;
    let typed_redaction_report = redact_turns_with_report(&mut turns);
    ensure_before_deadline(deadline)?;
    if turns.is_empty() {
        return Err(ImportError::NoImportableTurns.into());
    }
    if !facts.terminal
        && let Some(last) = turns.last_mut()
    {
        last.completeness = Completeness::Incomplete;
    }
    let (turn_boundaries, started_at, ended_at) =
        build_import_chronology(&facts, &turns, trusted_now)?;
    Ok(PreparedImportProjection {
        content_digest: source_digest(&turns),
        started_at,
        ended_at,
        session_state: if facts.terminal {
            "stopped".to_string()
        } else {
            "active".to_string()
        },
        stopped_at: facts.terminal.then_some(ended_at),
        storage_commitment,
        redaction_report: merged_import_redaction_report(
            &snapshot_redaction_report,
            &typed_redaction_report,
        ),
        transcript_snapshot: Some(transcript_snapshot),
        turn_boundaries,
        turns,
    })
}

/// Reconstruct the parent-owned compatibility fields after it has checked the
/// helper's fixed storage commitment and derived the opaque V2 source
/// commitment inside the scoped repository.
pub(crate) fn import_request_from_projection(
    agent_kind: AgentKind,
    provider_session_id: String,
    source_kind: String,
    source_commitment: String,
    working_dir: PathBuf,
    projection: PreparedImportProjection,
) -> ImportRequest {
    ImportRequest {
        agent_kind,
        session_id: build_ai_session_id(capture_provider_name(agent_kind), &provider_session_id),
        provider_session_id,
        source_kind,
        source_id: source_commitment.clone(),
        identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
        content_digest: projection.content_digest,
        started_at: projection.started_at,
        ended_at: projection.ended_at,
        session_state: projection.session_state,
        stopped_at: projection.stopped_at,
        working_dir,
        repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_string(),
        capture_scope: None,
        source_fingerprint: source_commitment,
        existing_session_fingerprint: None,
        redaction_report: projection.redaction_report,
        transcript_snapshot: projection.transcript_snapshot,
        turn_boundaries: projection.turn_boundaries,
        turns: projection.turns,
    }
}

pub(crate) fn identity_id(request: &ImportRequest) -> String {
    import_identity_id_for_parts(
        request.agent_kind,
        &request.provider_session_id,
        &request.source_kind,
        &request.source_id,
        request.identity_schema_version,
    )
}

fn import_identity_id_for_parts(
    agent_kind: AgentKind,
    provider_session_id: &str,
    source_kind: &str,
    source_id: &str,
    schema_version: i64,
) -> String {
    let mut digest = Sha256::new();
    let schema_version = schema_version.to_string();
    for value in [
        agent_kind.as_db_str(),
        provider_session_id,
        source_kind,
        source_id,
        schema_version.as_str(),
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("import-{}", hex::encode(digest.finalize()))
}

fn legacy_import_source_fingerprint(
    agent_kind: AgentKind,
    source_kind: &str,
    source_id: &str,
) -> String {
    hex::encode(Sha256::digest(
        format!("{}\0{}\0{}", agent_kind.as_db_str(), source_kind, source_id).as_bytes(),
    ))
}

fn legacy_import_repository_identity(storage_root: &Path) -> Result<String> {
    let canonical_storage = storage_root
        .canonicalize()
        .context("canonicalize legacy import repository storage proof")?;
    Ok(hex::encode(Sha256::digest(
        canonical_storage.to_string_lossy().as_bytes(),
    )))
}

pub(crate) fn is_import_source_commitment_v2(value: &str) -> bool {
    let Some(value) = value.strip_prefix("source/hmac-v2/") else {
        return false;
    };
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Reject a newly supplied mutable V1 request. Legacy rows remain readable
/// only while the scoped migration below proves and rewrites their mutable
/// ownership state; no generic import API may create another raw-locator or
/// unkeyed-SHA durable record.
fn assert_v2_durable_import_request(request: &ImportRequest) -> Result<()> {
    if request.identity_schema_version != IMPORT_IDENTITY_SCHEMA_VERSION_V2
        || !is_import_source_commitment_v2(&request.source_id)
        || request.source_fingerprint != request.source_id
        || request.repository_identity != IMPORT_DURABLE_IDENTITY_NOT_RETAINED
    {
        return Err(ImportError::SourceAuthorization.into());
    }
    Ok(())
}

/// Inputs that bind a legacy mutable import proof to the exact scoped V2
/// source commitment discovered by the descriptor-owned command path.
pub(crate) struct LegacyImportMigrationRequest<'a> {
    pub(crate) scope: &'a CaptureScope,
    pub(crate) storage_root: &'a Path,
    pub(crate) authorized_root: &'a Path,
    pub(crate) agent_kind: AgentKind,
    pub(crate) provider_session_id: &'a str,
    pub(crate) source_kind: &'a str,
    pub(crate) legacy_source_id: &'a str,
    pub(crate) v2_source_commitment: &'a str,
    /// Paired ingress deadline used for all mutable V1→V2 ownership work.
    pub(crate) deadline: CaptureCommitDeadline,
}

/// Migrate the *mutable* ownership proof for a historical V1 import after the
/// command has re-opened the exact source, revalidated its scope/root, and
/// derived a repository-keyed V2 commitment. Immutable checkpoint and trace
/// artifacts remain deliberately untouched as historical evidence.
///
/// Only a committed, ownerless, marker-quiescent V1 identity may move. A
/// writing/partial row or a live/repair-pending marker must recover as V1;
/// creating a second V2 identity in those states would make retries diverge.
pub(crate) async fn migrate_legacy_import_ownership_in_scope_until(
    conn: &DatabaseConnection,
    request: LegacyImportMigrationRequest<'_>,
) -> Result<bool> {
    let LegacyImportMigrationRequest {
        scope,
        storage_root,
        authorized_root,
        agent_kind,
        provider_session_id,
        source_kind,
        legacy_source_id,
        v2_source_commitment,
        deadline,
    } = request;
    ensure_before_deadline(deadline.monotonic())?;
    if !is_import_source_commitment_v2(v2_source_commitment) {
        return Err(ImportError::SourceAuthorization.into());
    }
    let migration = async {
        let resolved_scope = await_import_precommit_read_until(
            deadline.monotonic(),
            CaptureScope::resolve(conn, authorized_root),
        )
        .await
        .context("resolve scoped legacy import migration root")?;
        ensure_before_deadline(deadline.monotonic())?;
        if &resolved_scope != scope {
            return Err(ImportError::RepositoryConflict.into());
        }
        let authorized_storage = util::try_get_storage_path(Some(authorized_root.to_path_buf()))
            .context("resolve storage for scoped legacy import migration root")?
            .canonicalize()
            .context("canonicalize storage for scoped legacy import migration root")?;
        let supplied_storage = storage_root
            .canonicalize()
            .context("canonicalize scoped legacy import migration storage")?;
        if authorized_storage != supplied_storage {
            return Err(ImportError::RepositoryConflict.into());
        }
        let legacy_repository_identity = legacy_import_repository_identity(&supplied_storage)?;
        let legacy_source_fingerprint =
            legacy_import_source_fingerprint(agent_kind, source_kind, legacy_source_id);
        let legacy_identity_id = import_identity_id_for_parts(
            agent_kind,
            provider_session_id,
            source_kind,
            legacy_source_id,
            IMPORT_IDENTITY_SCHEMA_VERSION_V1,
        );
        let v2_identity_id = import_identity_id_for_parts(
            agent_kind,
            provider_session_id,
            source_kind,
            v2_source_commitment,
            IMPORT_IDENTITY_SCHEMA_VERSION_V2,
        );
        let session = catalog_session_for_parts(agent_kind, provider_session_id, authorized_root)?;
        let legacy_catalog_source = CaptureImportSource::new(
            source_kind,
            legacy_source_id,
            legacy_repository_identity,
            legacy_source_fingerprint,
            IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            None,
        )
        .map_err(|error| anyhow!("prepare legacy import ownership proof: {error}"))?;
        let v2_catalog_source = CaptureImportSource::new(
            source_kind,
            v2_source_commitment,
            IMPORT_DURABLE_IDENTITY_NOT_RETAINED,
            v2_source_commitment,
            IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            None,
        )
        .map_err(|error| anyhow!("prepare V2 import ownership proof: {error}"))?;
        ensure_before_deadline(deadline.monotonic())?;
        let txn = begin_import_write_transaction_until(
            conn,
            deadline,
            "begin scoped legacy import ownership migration",
        )
        .await?;
        let result = async {
            await_import_precommit_read_until(
                deadline.monotonic(),
                scope.assert_workspace_fence_live(&txn),
            )
            .await
            .context("verify capture workspace lease before migrating legacy import ownership")?;
            ensure_before_deadline(deadline.monotonic())?;
            // A provider session must have only one legacy mutable proof.
            // Even a committed exact match cannot be safely migrated while a
            // V1 row with another locator *or source kind* is present: the
            // descriptor may have been swapped and all such rows share the
            // same catalog session identity. Minting V2 would leave divergent
            // retry namespaces. Do not reveal the other proof in errors.
            let mismatched_v1 = await_import_precommit_read_until(deadline.monotonic(), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_import_identity
                     WHERE agent_kind = ? AND provider_session_id = ?
                       AND schema_version = ?
                       AND (source_kind <> ? OR source_id <> ?)
                     LIMIT 1",
                    [
                        agent_kind.as_db_str().into(),
                        provider_session_id.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V1.into(),
                        source_kind.into(),
                        legacy_source_id.into(),
                    ],
                ))
                .await
                .context("check for conflicting legacy import proof")
            })
            .await?;
            if mismatched_v1.is_some() {
                return Err(ImportError::RepositoryConflict.into());
            }
            let v1 = await_import_precommit_read_until(deadline.monotonic(), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT identity_id, state, owner, lease_expires_at, attempt_id,
                            attempt_checkpoint_id, scope_state, repo_id, worktree_id,
                            workspace_id, workspace_fence
                     FROM agent_import_identity
                     WHERE agent_kind = ? AND provider_session_id = ?
                       AND source_kind = ? AND source_id = ? AND schema_version = ?",
                    [
                        agent_kind.as_db_str().into(),
                        provider_session_id.into(),
                        source_kind.into(),
                        legacy_source_id.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V1.into(),
                    ],
                ))
                .await
                .context("read legacy import identity for scoped migration")
            })
            .await?;
            let Some(v1) = v1 else {
                return Ok(false);
            };
            let identity_id: String = v1.try_get_by("identity_id")?;
            let state: String = v1.try_get_by("state")?;
            let owner: Option<String> = v1.try_get_by("owner")?;
            let lease_expires_at: Option<i64> = v1.try_get_by("lease_expires_at")?;
            let attempt_id: Option<String> = v1.try_get_by("attempt_id")?;
            let attempt_checkpoint_id: Option<String> = v1.try_get_by("attempt_checkpoint_id")?;
            let scope_state: String = v1.try_get_by("scope_state")?;
            let repo_id: Option<String> = v1.try_get_by("repo_id")?;
            let worktree_id: Option<String> = v1.try_get_by("worktree_id")?;
            let workspace_id: Option<String> = v1.try_get_by("workspace_id")?;
            let workspace_fence: Option<i64> = v1.try_get_by("workspace_fence")?;
            if identity_id != legacy_identity_id
                || state != "committed"
                || owner.is_some()
                || lease_expires_at.is_some()
                || scope_state != "scoped"
                || repo_id.as_deref() != Some(scope.repo_id.as_str())
                || worktree_id.as_deref() != Some(scope.worktree_id.as_str())
                || workspace_id != scope.workspace_id
                || workspace_fence != scope.workspace_fence
            {
                return Err(ImportError::RepositoryConflict.into());
            }
            // Final-turn commits intentionally retain their terminal attempt
            // checkpoint. It is quiescent only when both fields name that
            // exact durable checkpoint; a half-bound or missing object is a
            // recovery state and must remain V1 rather than spawning V2.
            match (attempt_id.as_deref(), attempt_checkpoint_id.as_deref()) {
                (None, None) => {}
                (Some(attempt_id), Some(checkpoint_id)) if attempt_id == checkpoint_id => {
                    let durable = await_import_precommit_read_until(deadline.monotonic(), async {
                        txn.query_one_raw(Statement::from_sql_and_values(
                            txn.get_database_backend(),
                            "SELECT 1 FROM agent_checkpoint
                             WHERE checkpoint_id = ? AND session_id = ? AND scope = 'committed'",
                            [checkpoint_id.into(), session.session_id().into()],
                        ))
                        .await
                        .context("verify terminal legacy import checkpoint before migration")
                    })
                    .await?;
                    if durable.is_none() {
                        return Err(ImportError::RepositoryConflict.into());
                    }
                }
                _ => return Err(ImportError::RepositoryConflict.into()),
            }
            ensure_before_deadline(deadline.monotonic())?;
            // V2 ownership shares the same provider-session catalog row as
            // V1. A differently keyed V2 row is as unsafe as a second V1
            // locator: relabelling this V1 proof would leave two mutable V2
            // retry namespaces for one provider session. Keep the other
            // commitment out of the error surface.
            let mismatched_v2 = await_import_precommit_read_until(deadline.monotonic(), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_import_identity
                     WHERE agent_kind = ? AND provider_session_id = ?
                       AND schema_version = ?
                       AND (source_kind <> ? OR source_id <> ?)
                     LIMIT 1",
                    [
                        agent_kind.as_db_str().into(),
                        provider_session_id.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V2.into(),
                        source_kind.into(),
                        v2_source_commitment.into(),
                    ],
                ))
                .await
                .context("check for conflicting V2 import proof")
            })
            .await?;
            if mismatched_v2.is_some() {
                return Err(ImportError::RepositoryConflict.into());
            }
            let v2_exists = await_import_precommit_read_until(deadline.monotonic(), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_import_identity
                     WHERE agent_kind = ? AND provider_session_id = ?
                       AND source_kind = ? AND source_id = ? AND schema_version = ?",
                    [
                        agent_kind.as_db_str().into(),
                        provider_session_id.into(),
                        source_kind.into(),
                        v2_source_commitment.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V2.into(),
                    ],
                ))
                .await
                .context("check for conflicting V2 import identity")
            })
            .await?;
            if v2_exists.is_some() {
                return Err(ImportError::RepositoryConflict.into());
            }
            let marker = await_import_precommit_read_until(deadline.monotonic(), async {
                MetadataKv::get_with_conn(
                    &txn,
                    MetadataScope::AgentImportIndexRepair,
                    session.session_id(),
                    IMPORT_INDEX_REPAIR_MARKER_KEY,
                )
                .await
                .context("read legacy import object-index repair marker")
            })
            .await?;
            let mut marker_to_migrate = match marker {
                None => None,
                Some(marker) => {
                    let marker: ImportIndexRepairMarker = serde_json::from_str(&marker.value)
                        .map_err(|_| ImportError::RepositoryConflict)?;
                    // Decode alone is not enough: a partially-written marker
                    // with omitted ownership fields must never satisfy the
                    // expiry predicate and be re-bound as V2.
                    validate_import_index_repair_marker(&marker)
                        .map_err(|_| ImportError::RepositoryConflict)?;
                    let now_ms = Utc::now().timestamp_millis();
                    if marker.schema_version != IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V1
                        || marker.identity_id != legacy_identity_id
                        || marker.agent_kind != agent_kind.as_db_str()
                        || marker.provider_session_id != provider_session_id
                        || marker.source_kind != source_kind
                        || marker.source_id != legacy_source_id
                        || marker.capture_scope.as_ref() != Some(scope)
                        || marker.state != "active"
                        || marker.lease_expires_at > now_ms
                    {
                        return Err(ImportError::RepositoryConflict.into());
                    }
                    Some(marker)
                }
            };
            ensure_before_deadline(deadline.monotonic())?;
            CaptureCatalogStore::migrate_import_session_ownership(
                &txn,
                scope,
                &session,
                &legacy_catalog_source,
                &v2_catalog_source,
            )
            .await
            .map_err(|error| {
                map_import_catalog_error("migrate legacy import catalog ownership", error)
            })?;
            ensure_before_deadline(deadline.monotonic())?;
            let updated = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_import_identity
                     SET identity_id = ?, source_id = ?, schema_version = ?, updated_at = ?
                     WHERE identity_id = ? AND agent_kind = ? AND provider_session_id = ?
                       AND source_kind = ? AND source_id = ? AND schema_version = ?
                       AND state = 'committed' AND owner IS NULL AND lease_expires_at IS NULL
                       AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                       AND workspace_id IS ? AND workspace_fence IS ?",
                    [
                        v2_identity_id.clone().into(),
                        v2_source_commitment.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V2.into(),
                        Utc::now().timestamp_millis().into(),
                        legacy_identity_id.into(),
                        agent_kind.as_db_str().into(),
                        provider_session_id.into(),
                        source_kind.into(),
                        legacy_source_id.into(),
                        IMPORT_IDENTITY_SCHEMA_VERSION_V1.into(),
                        scope.repo_id.clone().into(),
                        scope.worktree_id.clone().into(),
                        scope.workspace_id.clone().into(),
                        scope.workspace_fence.into(),
                    ],
                ))
                .await
                // The command deliberately reduces an unexpected mutation
                // failure to the same safe, retryable conflict surface as a
                // raced legacy proof.  Preserve that typed sentinel here as
                // well so in-process callers cannot accidentally expose a
                // partially actionable migration result.
                .map_err(|_| anyhow::Error::from(ImportError::RepositoryConflict))?;
            if updated.rows_affected() != 1 {
                return Err(ImportError::LeaseBusy.into());
            }
            if let Some(marker) = marker_to_migrate.as_mut() {
                marker.schema_version = IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2;
                marker.identity_id = v2_identity_id;
                marker.source_id = v2_source_commitment.to_string();
                let value = serde_json::to_string(marker)
                    .context("encode migrated import object-index repair marker")?;
                ensure_before_deadline(deadline.monotonic())?;
                MetadataKv::set_with_conn(
                    &txn,
                    MetadataScope::AgentImportIndexRepair,
                    session.session_id(),
                    IMPORT_INDEX_REPAIR_MARKER_KEY,
                    &value,
                    crate::internal::metadata::MetadataValueType::Text,
                )
                .await
                .context("migrate import object-index repair marker to V2")?;
            }
            Ok(true)
        }
        .await;
        match result {
            Ok(true) => {
                commit_import_transaction_until(
                    txn,
                    scope,
                    deadline,
                    "commit scoped legacy import ownership migration",
                )
                .await?;
                Ok(true)
            }
            Ok(false) => {
                txn.rollback().await.ok();
                Ok(false)
            }
            Err(error) => {
                txn.rollback().await.ok();
                Err(error)
            }
        }
    };
    migration.await
}

fn import_identity_schema_version(request: &ImportRequest) -> Result<i64> {
    match request.identity_schema_version {
        IMPORT_IDENTITY_SCHEMA_VERSION_V1 | IMPORT_IDENTITY_SCHEMA_VERSION_V2 => {
            Ok(request.identity_schema_version)
        }
        _ => bail!("import identity schema version is invalid"),
    }
}

async fn capture_scope_for_request<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
) -> Result<CaptureScope> {
    match &request.capture_scope {
        Some(scope) => Ok(scope.clone()),
        // Direct in-process callers predate W4's command-owned scope handoff.
        // They are test/internal compatibility seams only; production command
        // paths set `capture_scope` from the real worktree before parsing.
        None => CaptureScope::main_for_connection(conn).await,
    }
}

async fn capture_scope_for_request_until<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
    deadline: Instant,
) -> Result<CaptureScope> {
    await_import_precommit_read_until(deadline, capture_scope_for_request(conn, request)).await
}

fn catalog_session_for_parts(
    agent_kind: AgentKind,
    provider_session_id: &str,
    working_dir: &Path,
) -> Result<CaptureCatalogSession> {
    CaptureCatalogSession::new(
        build_ai_session_id(capture_provider_name(agent_kind), provider_session_id),
        agent_kind.as_db_str(),
        provider_session_id,
        working_dir.to_string_lossy().into_owned(),
    )
    .map_err(|error| anyhow!("prepare historical import catalog session: {error}"))
}

fn catalog_session_for_request(request: &ImportRequest) -> Result<CaptureCatalogSession> {
    let session = catalog_session_for_parts(
        request.agent_kind,
        &request.provider_session_id,
        &request.working_dir,
    )?;
    // `session_id` is normally deterministically reconstructed above. Keep a
    // strict equality check so a forged direct request cannot make catalog
    // ownership and import identity refer to different sessions.
    if session.session_id() != request.session_id {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    Ok(session)
}

fn catalog_import_source_for_request(request: &ImportRequest) -> Result<CaptureImportSource> {
    CaptureImportSource::new(
        request.source_kind.clone(),
        request.source_id.clone(),
        request.repository_identity.clone(),
        request.source_fingerprint.clone(),
        request.identity_schema_version,
        request.transcript_snapshot.clone(),
    )
    .map_err(|error| anyhow!("prepare historical import source ownership: {error}"))
}

fn catalog_import_redaction_report_for_request(
    request: &ImportRequest,
) -> Result<CaptureCatalogRedactionReport> {
    CaptureCatalogRedactionReport::from_import_value(&request.redaction_report)
        .map_err(|error| anyhow!("prepare historical import redaction report: {error}"))
}

fn map_import_catalog_error(context: &'static str, error: CaptureCatalogError) -> anyhow::Error {
    match error {
        CaptureCatalogError::Tombstoned => ImportError::Erased.into(),
        CaptureCatalogError::ScopeRejected | CaptureCatalogError::ImportSessionConflict => {
            ImportError::RepositoryConflict.into()
        }
        error => anyhow!("{context}: {error}"),
    }
}

fn existing_session_snapshot_fingerprint(snapshot: &ExistingSessionOwnershipSnapshot) -> String {
    let mut digest = Sha256::new();
    for value in [
        snapshot.session_id.as_str(),
        snapshot.agent_kind.as_str(),
        snapshot.provider_session_id.as_str(),
        snapshot.working_dir.as_str(),
        snapshot.metadata_json.as_str(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    hex::encode(digest.finalize())
}

pub async fn load_existing_session_ownership<C: ConnectionTrait>(
    conn: &C,
    kind: AgentKind,
    provider_session_id: &str,
) -> Result<Option<ExistingSessionOwnershipSnapshot>> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT session_id, agent_kind, provider_session_id, working_dir, metadata_json
             FROM agent_session WHERE agent_kind = ? AND provider_session_id = ?",
            [kind.as_db_str().into(), provider_session_id.into()],
        ))
        .await
        .context("query existing agent import ownership snapshot")?;
    row.map(|row| {
        Ok(ExistingSessionOwnershipSnapshot {
            session_id: row.try_get_by("session_id")?,
            agent_kind: row.try_get_by("agent_kind")?,
            provider_session_id: row.try_get_by("provider_session_id")?,
            working_dir: row.try_get_by("working_dir")?,
            metadata_json: row.try_get_by("metadata_json")?,
        })
    })
    .transpose()
}

/// Scope-aware import ownership lookup used by the command path. The legacy
/// public helper remains for preparation tests, while every real import first
/// rejects a foreign/unknown provider-session claim and then reads only the
/// exact canonical scope.
pub async fn load_existing_session_ownership_in_scope<C: ConnectionTrait>(
    conn: &C,
    kind: AgentKind,
    provider_session_id: &str,
    scope: &CaptureScope,
) -> Result<Option<ExistingSessionOwnershipSnapshot>> {
    scope
        .assert_provider_session_compatible(conn, provider_session_id)
        .await?;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT session_id, agent_kind, provider_session_id, working_dir, metadata_json
             FROM agent_session
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                kind.as_db_str().into(),
                provider_session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("query scoped agent import ownership snapshot")?;
    row.map(|row| {
        Ok(ExistingSessionOwnershipSnapshot {
            session_id: row.try_get_by("session_id")?,
            agent_kind: row.try_get_by("agent_kind")?,
            provider_session_id: row.try_get_by("provider_session_id")?,
            working_dir: row.try_get_by("working_dir")?,
            metadata_json: row.try_get_by("metadata_json")?,
        })
    })
    .transpose()
}

/// Validate a pre-query row returned by
/// [`load_existing_session_ownership_in_scope`]. That scoped lookup is the
/// storage/worktree proof: the persisted catalog path is retained solely for
/// the legacy `agent_session.working_dir` contract, never reopened or
/// resolved by the parent after its descriptor-owning helper exits.
///
/// The parent retains only an exact row fingerprint for race-safe,
/// filesystem-free revalidation during lease acquisition.
pub(crate) fn validate_scoped_prepared_existing_session(
    request: &mut ImportRequest,
    snapshot: Option<&ExistingSessionOwnershipSnapshot>,
) -> Result<()> {
    let Some(snapshot) = snapshot else {
        request.existing_session_fingerprint = None;
        return Ok(());
    };
    if snapshot.session_id != request.session_id
        || snapshot.agent_kind != request.agent_kind.as_db_str()
        || snapshot.provider_session_id != request.provider_session_id
    {
        return Err(ImportError::RepositoryConflict.into());
    }
    let metadata: serde_json::Value = serde_json::from_str(&snapshot.metadata_json)
        .context("parse existing agent session ownership metadata")?;
    if request.identity_schema_version == IMPORT_IDENTITY_SCHEMA_VERSION_V2 {
        // A V2 import may legitimately join a live-captured session that has
        // no import ownership metadata yet. Scope/session ownership above is
        // the proof in that case. Once any import ownership field exists,
        // however, require the entire V2 tuple so a partial or swapped import
        // record can never be adopted as a live-session compatibility case.
        let import_fields = [
            ("repository_identity", request.repository_identity.as_str()),
            ("source_kind", request.source_kind.as_str()),
            ("source_id", request.source_id.as_str()),
            ("source_fingerprint", request.source_fingerprint.as_str()),
        ];
        let has_import_ownership = import_fields
            .iter()
            .any(|(field, _)| metadata.get(*field).is_some())
            || metadata.get("import_source_schema_version").is_some();
        if has_import_ownership {
            for (field, expected) in import_fields {
                if metadata.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                    return Err(ImportError::RepositoryConflict.into());
                }
            }
            if metadata
                .get("import_source_schema_version")
                .and_then(serde_json::Value::as_i64)
                != Some(IMPORT_IDENTITY_SCHEMA_VERSION_V2)
            {
                return Err(ImportError::RepositoryConflict.into());
            }
        }
    }

    // A scoped live row can legitimately be rooted below the repository root.
    // Its scope was matched by the lookup above, so validate its bounded
    // catalog text and exact lexical normal form here. The raw spelling feeds
    // Claude's project-directory slug in a later bounded helper; accepting a
    // trailing or repeated separator would silently change that lookup.
    // Resolving this persisted path in the parent would reopen untrusted
    // filesystem I/O after the descriptor-owning helper has exited.
    let raw_existing_cwd = Path::new(&snapshot.working_dir);
    if !raw_existing_cwd.is_absolute() {
        return Err(ImportError::RepositoryConflict.into());
    }
    let mut existing_cwd = PathBuf::new();
    for component in raw_existing_cwd.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(ImportError::RepositoryConflict.into());
        }
        existing_cwd.push(component.as_os_str());
    }
    if existing_cwd.as_os_str() != raw_existing_cwd.as_os_str() {
        return Err(ImportError::RepositoryConflict.into());
    }
    catalog_session_for_parts(
        request.agent_kind,
        &request.provider_session_id,
        &existing_cwd,
    )
    .map_err(|_| ImportError::RepositoryConflict)?;
    if request.identity_schema_version == IMPORT_IDENTITY_SCHEMA_VERSION_V1 {
        for (field, expected) in [
            ("repository_identity", request.repository_identity.as_str()),
            ("source_kind", request.source_kind.as_str()),
            ("source_id", request.source_id.as_str()),
            ("source_fingerprint", request.source_fingerprint.as_str()),
        ] {
            if let Some(actual) = metadata.get(field).and_then(serde_json::Value::as_str)
                && actual != expected
            {
                return Err(ImportError::RepositoryConflict.into());
            }
        }
    }
    request.working_dir = existing_cwd;
    request.existing_session_fingerprint = Some(existing_session_snapshot_fingerprint(snapshot));
    Ok(())
}

async fn validate_existing_session_ownership<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
    deadline: Instant,
) -> Result<()> {
    let scope = capture_scope_for_request_until(conn, request, deadline).await?;
    let observed = await_import_precommit_read_until(
        deadline,
        load_existing_session_ownership_in_scope(
            conn,
            request.agent_kind,
            &request.provider_session_id,
            &scope,
        ),
    )
    .await?;
    let observed_fingerprint = observed.as_ref().map(existing_session_snapshot_fingerprint);
    if observed_fingerprint != request.existing_session_fingerprint {
        return Err(ImportError::RepositoryConflict.into());
    }
    Ok(())
}

async fn ensure_session(
    txn: &sea_orm::DatabaseTransaction,
    request: &ImportRequest,
    scope: &CaptureScope,
) -> Result<()> {
    let catalog_request = CaptureImportSessionPrepareRequest::new(
        scope.clone(),
        catalog_session_for_request(request)?,
        catalog_import_source_for_request(request)?,
        catalog_import_redaction_report_for_request(request)?,
        request.started_at,
        request.existing_session_fingerprint.clone(),
    )
    .map_err(|error| {
        map_import_catalog_error("prepare historical import catalog request", error)
    })?;
    CaptureCatalogStore::prepare_import_session(txn, &catalog_request)
        .await
        .map_err(|error| {
            map_import_catalog_error("prepare historical import catalog session", error)
        })?;
    Ok(())
}

async fn assert_import_identity_scope<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
    scope: &CaptureScope,
) -> Result<()> {
    let schema_version = import_identity_schema_version(request)?;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_import_identity
             WHERE agent_kind = ? AND provider_session_id = ?
               AND source_kind = ? AND source_id = ? AND schema_version = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                schema_version.into(),
            ],
        ))
        .await
        .context("read import-identity workspace ownership")?;
    let Some(row) = row else {
        return Ok(());
    };
    let scope_state: String = row.try_get_by("scope_state")?;
    let repo_id: Option<String> = row.try_get_by("repo_id")?;
    let worktree_id: Option<String> = row.try_get_by("worktree_id")?;
    let workspace_id: Option<String> = row.try_get_by("workspace_id")?;
    let workspace_fence: Option<i64> = row.try_get_by("workspace_fence")?;
    if scope_state != "scoped"
        || repo_id.as_deref() != Some(scope.repo_id.as_str())
        || worktree_id.as_deref() != Some(scope.worktree_id.as_str())
        || workspace_id != scope.workspace_id
        || workspace_fence != scope.workspace_fence
    {
        bail!(
            "import identity belongs to a legacy or different workspace scope; refusing to \
             take over it — inspect it with `libra worktree doctor` before retrying"
        );
    }
    Ok(())
}

async fn acquire_identity(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<ImportLease> {
    let identity_id = identity_id(request);
    let schema_version = import_identity_schema_version(request)?;
    let scope = capture_scope_for_request_until(conn, request, deadline.monotonic()).await?;
    let lease_expires_at = now_ms
        .checked_add(IMPORT_LEASE_MS)
        .context("import lease timestamp overflow")?;
    let txn =
        begin_import_write_transaction_until(conn, deadline, "begin import identity lease").await?;
    if let Err(error) = await_import_precommit_read_until(
        deadline.monotonic(),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before acquiring import identity");
    }
    // Acquire SQLite's writer slot before checking the tombstone and the
    // prepared session fingerprint. Otherwise erasure can commit between a
    // standalone ownership read and this transaction, turning a known erased
    // identity into a misleading repository-conflict result.
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify import deadline before serializing identity acquisition");
    }
    let serialized = CaptureCatalogStore::serialize_import_with_erase(
        &txn,
        &scope,
        &catalog_session_for_request(request)?,
    )
    .await
    .map_err(|error| {
        map_import_catalog_error(
            "serialize import identity acquisition with session erasure",
            error,
        )
    });
    if let Err(error) = serialized {
        txn.rollback().await.ok();
        return Err(error);
    }
    let tombstone = match await_import_precommit_read_until(deadline.monotonic(), async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check import tombstone")
    })
    .await
    {
        Ok(tombstone) => tombstone,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if tombstone.is_some() {
        txn.rollback().await.ok();
        return Err(ImportError::Erased.into());
    }
    if let Err(error) =
        validate_existing_session_ownership(&txn, request, deadline.monotonic()).await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    // Keep the write barrier and session insert under the same SQLite writer
    // transaction. Otherwise erase can commit a tombstone between the check
    // and a standalone session insert, leaving a locally erased session
    // resurrected even though identity acquisition fails closed.
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before preparing import session");
    }
    if let Err(error) = ensure_session(&txn, request, &scope).await {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = await_import_precommit_read_until(
        deadline.monotonic(),
        assert_import_identity_scope(&txn, request, &scope),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before inserting identity lease");
    }
    let inserted = match txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind,
                source_id, schema_version, observed_digest, next_ordinal,
                state, owner, lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 0, 'leased', ?, ?, 1, ?, ?, ?, ?, ?, ?, 'scoped')
             ON CONFLICT(agent_kind, provider_session_id, source_kind, source_id, schema_version)
             DO NOTHING",
            [
                identity_id.clone().into(),
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                schema_version.into(),
                request.content_digest.clone().into(),
                owner.into(),
                lease_expires_at.into(),
                now_ms.into(),
                now_ms.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("insert import identity lease")
    {
        Ok(inserted) => inserted,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let fence_token = if inserted.rows_affected() == 1 {
        1
    } else {
        let row = match await_import_precommit_read_until(deadline.monotonic(), async {
            txn.query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT identity_id, owner, lease_expires_at, fence_token,
                        attempt_checkpoint_id
                 FROM agent_import_identity
                 WHERE agent_kind = ? AND provider_session_id = ?
                   AND source_kind = ? AND source_id = ? AND schema_version = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?",
                [
                    request.agent_kind.as_db_str().into(),
                    request.provider_session_id.clone().into(),
                    request.source_kind.clone().into(),
                    request.source_id.clone().into(),
                    schema_version.into(),
                    scope.repo_id.clone().into(),
                    scope.worktree_id.clone().into(),
                    scope.workspace_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .context("read import identity lease")
        })
        .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                txn.rollback().await.ok();
                return Err(anyhow!(
                    "import identity disappeared during lease acquisition"
                ));
            }
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error);
            }
        };
        let row_identity_id: String = row.try_get_by("identity_id")?;
        let existing_owner: Option<String> = row.try_get_by("owner")?;
        let existing_expiry: Option<i64> = row.try_get_by("lease_expires_at")?;
        let existing_fence: Option<i64> = row.try_get_by("fence_token")?;
        let stale_attempt_checkpoint_id: Option<String> =
            row.try_get_by("attempt_checkpoint_id")?;
        if existing_owner.as_deref() != Some(owner)
            && existing_expiry.is_some_and(|expiry| expiry > now_ms)
        {
            txn.rollback().await.ok();
            return Err(ImportError::LeaseBusy.into());
        }
        let next_fence = existing_fence
            .unwrap_or(0)
            .checked_add(1)
            .context("import identity fence overflow")?;
        if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
            txn.rollback().await.ok();
            return Err(error).context("verify import deadline before taking over identity lease");
        }
        let updated = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_import_identity
                 SET state = 'leased', owner = ?, lease_expires_at = ?,
                     fence_token = ?, observed_digest = ?, last_error_code = NULL,
                     attempt_id = NULL, attempt_checkpoint_id = NULL,
                     updated_at = ?
                 WHERE identity_id = ? AND fence_token IS ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?
                   AND (agent_import_identity.workspace_id IS NULL OR EXISTS (
                       SELECT 1 FROM workspace_record
                       WHERE workspace_id = agent_import_identity.workspace_id
                         AND repo_id = agent_import_identity.repo_id
                         AND lease_fence = agent_import_identity.workspace_fence
                         AND state IN ('provisioning', 'active', 'releasing')
                         AND lease_owner IS NOT NULL
                         AND lease_expires_at > (unixepoch('now') * 1000)
                   ))",
                [
                    owner.into(),
                    lease_expires_at.into(),
                    next_fence.into(),
                    request.content_digest.clone().into(),
                    now_ms.into(),
                    row_identity_id.into(),
                    Value::from(existing_fence),
                    scope.repo_id.clone().into(),
                    scope.worktree_id.clone().into(),
                    scope.workspace_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .context("take over import identity lease")?;
        if updated.rows_affected() != 1 {
            txn.rollback().await.ok();
            return Err(ImportError::LeaseBusy.into());
        }
        if let Some(stale_owner) = existing_owner.as_deref().filter(|stale| *stale != owner) {
            if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
                txn.rollback().await.ok();
                return Err(error).context(
                    "verify import deadline before abandoning stale coverage reservations",
                );
            }
            coverage_gate::abandon_reserved_turn_claims_with_conn(
                &txn,
                &request.session_id,
                stale_owner,
                "import",
                now_ms,
            )
            .await
            .context("abandon crashed import owner's stale coverage reservations")?;
        }
        if let Some(stale_checkpoint_id) = stale_attempt_checkpoint_id {
            if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
                txn.rollback().await.ok();
                return Err(error)
                    .context("verify import deadline before retiring stale import marker");
            }
            ImportedCheckpointWriter::retire_stale_prebound_attempt(
                &txn,
                &request.session_id,
                &stale_checkpoint_id,
            )
            .await
            .map_err(|error| anyhow!("retire fenced crashed import attempt marker: {error}"))?;
        }
        next_fence
    };
    commit_import_transaction_until(txn, &scope, deadline, "commit import identity lease").await?;
    Ok(ImportLease {
        identity_id,
        owner: owner.to_string(),
        fence_token,
    })
}

async fn bind_attempt(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    claim: &ReservedTurnClaim,
    checkpoint_id: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<TracesInflightMarker> {
    ensure_before_deadline(deadline.monotonic())?;
    let scope = capture_scope_for_request_until(conn, request, deadline.monotonic()).await?;
    let txn = begin_import_write_transaction_until(conn, deadline, "begin import attempt binding")
        .await?;
    if let Err(error) = await_import_precommit_read_until(
        deadline.monotonic(),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error).context("verify capture workspace lease before binding import attempt");
    }
    let lease_expires_at = now_ms
        .checked_add(IMPORT_LEASE_MS)
        .context("import lease timestamp overflow during attempt binding")?;
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before binding import identity attempt");
    }
    let identity = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_import_identity
             SET state = 'writing', attempt_id = ?, attempt_checkpoint_id = ?,
                 lease_expires_at = ?, updated_at = ?
             WHERE identity_id = ? AND owner = ? AND fence_token = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND (agent_import_identity.workspace_id IS NULL OR EXISTS (
                   SELECT 1 FROM workspace_record
                   WHERE workspace_id = agent_import_identity.workspace_id
                     AND repo_id = agent_import_identity.repo_id
                     AND lease_fence = agent_import_identity.workspace_fence
                     AND state IN ('provisioning', 'active', 'releasing')
                     AND lease_owner IS NOT NULL
                     AND lease_expires_at > (unixepoch('now') * 1000)
               ))
               AND state IN ('leased','writing')",
            [
                checkpoint_id.into(),
                checkpoint_id.into(),
                lease_expires_at.into(),
                now_ms.into(),
                lease.identity_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence_token.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("bind import identity attempt")?;
    if identity.rows_affected() != 1 {
        txn.rollback().await.ok();
        return Err(ImportError::LeaseBusy.into());
    }
    // One source may contain hundreds of turns. Refresh every still-owned
    // reservation before constructing the next object so a healthy long
    // import does not lose later claims merely because their original
    // reservation was acquired more than one lease interval ago. A live
    // writer that already preempted a claim changed its owner/fence and is
    // intentionally untouched.
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before renewing coverage reservations");
    }
    coverage_gate::renew_import_turn_claim_leases_with_conn(
        &txn,
        &request.session_id,
        &lease.owner,
        lease_expires_at,
        now_ms,
    )
    .await
    .context("renew remaining import coverage reservations")?;
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before binding import coverage attempt");
    }
    let coverage_bound = coverage_gate::bind_import_turn_claim_attempt_with_conn(
        &txn,
        &request.session_id,
        claim,
        &lease.owner,
        checkpoint_id,
        now_ms,
    )
    .await
    .context("bind import coverage attempt")?;
    if !coverage_bound {
        txn.rollback().await.ok();
        bail!("import coverage claim was fenced out before object construction");
    }
    // The attempt marker and the tombstone/fence checks above share one
    // SQLite writer transaction. An erase therefore either observes this
    // marker before reporting success or wins first and prevents all object
    // construction for the stale importer.
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before binding import traces marker");
    }
    let marker = ImportedCheckpointWriter::bind_prebound_attempt(
        &txn,
        &request.session_id,
        checkpoint_id,
        now_ms,
    )
    .await
    .map_err(|error| {
        anyhow!("write import traces in-flight marker in attempt transaction: {error}")
    })?;
    commit_import_transaction_until(txn, &scope, deadline, "commit import attempt binding").await?;
    Ok(marker)
}

/// Assemble the import-only lifecycle companion that the checkpoint store
/// applies inside the same ref-CAS transaction as coverage/catalog updates.
/// This is deliberately a typed request rather than ad-hoc state/JSON at
/// each writer callsite: import is the sole path allowed to reactivate a
/// terminal session by clearing `stopped_at` after it has proved newer
/// nonterminal source activity.
fn import_session_commit(
    request: &ImportRequest,
    scope: &CaptureScope,
) -> Result<CaptureImportSessionCommit> {
    let state = match (&*request.session_state, request.stopped_at) {
        ("active", None) => CaptureImportSessionLifecycleState::Active,
        ("stopped", Some(_)) => CaptureImportSessionLifecycleState::Stopped,
        ("active" | "stopped", _) => bail!(
            "historical import lifecycle state '{}' has an incompatible stopped timestamp",
            request.session_state
        ),
        _ => bail!(
            "unsupported historical import lifecycle state '{}'",
            request.session_state
        ),
    };
    CaptureImportSessionCommit::new(
        scope.clone(),
        catalog_session_for_request(request)?,
        catalog_import_source_for_request(request)?,
        catalog_import_redaction_report_for_request(request)?,
        state,
        request.started_at,
        request.ended_at,
        request.stopped_at,
    )
    .map_err(|error| anyhow!("prepare imported session lifecycle: {error}"))
}

async fn finalize_noop_identity(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    state: &str,
    last_error_code: Option<&str>,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let scope = capture_scope_for_request_until(conn, request, deadline.monotonic()).await?;
    let txn =
        begin_import_write_transaction_until(conn, deadline, "begin import identity finalization")
            .await?;
    if let Err(error) = await_import_precommit_read_until(
        deadline.monotonic(),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before finalizing import identity");
    }
    let tombstone = match await_import_precommit_read_until(deadline.monotonic(), async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check import tombstone during finalization")
    })
    .await
    {
        Ok(tombstone) => tombstone,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if tombstone.is_some() {
        txn.rollback().await.ok();
        return Err(ImportError::Erased.into());
    }
    let committed_digest: Value = if state == "committed" {
        Some(request.content_digest.clone()).into()
    } else {
        None::<String>.into()
    };
    if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
        txn.rollback().await.ok();
        return Err(error).context("verify import deadline before finalizing import identity");
    }
    let result = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_import_identity
             SET state = ?, committed_digest = COALESCE(?, committed_digest),
                 next_ordinal = CASE WHEN ? THEN ? ELSE next_ordinal END,
                 owner = NULL, lease_expires_at = NULL,
                 last_error_code = ?, updated_at = ?
             WHERE identity_id = ? AND owner = ? AND fence_token = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND (agent_import_identity.workspace_id IS NULL OR EXISTS (
                   SELECT 1 FROM workspace_record
                   WHERE workspace_id = agent_import_identity.workspace_id
                     AND repo_id = agent_import_identity.repo_id
                     AND lease_fence = agent_import_identity.workspace_fence
                     AND state IN ('provisioning', 'active', 'releasing')
                     AND lease_owner IS NOT NULL
                     AND lease_expires_at > (unixepoch('now') * 1000)
               ))",
            [
                state.into(),
                committed_digest,
                (state == "committed").into(),
                i64::try_from(request.turns.len())
                    .context("turn count exceeds import cursor range")?
                    .into(),
                last_error_code.into(),
                now_ms.into(),
                lease.identity_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence_token.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("finalize import identity")?;
    if result.rows_affected() != 1 {
        txn.rollback().await.ok();
        return Err(ImportError::LeaseBusy.into());
    }
    if state == "committed" {
        if let Err(error) = ensure_before_deadline(deadline.monotonic()) {
            txn.rollback().await.ok();
            return Err(error).context("verify import deadline before committing import lifecycle");
        }
        let session = import_session_commit(request, &scope)?;
        CaptureCatalogStore::apply_import_session_lifecycle(&txn, &session)
            .await
            .map_err(|error| {
                map_import_catalog_error("commit imported session lifecycle", error)
            })?;
    }
    commit_import_transaction_until(txn, &scope, deadline, "commit import identity finalization")
        .await?;
    Ok(())
}

async fn abandon_import_attempt(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    abandonment: ImportAttemptAbandonment<'_>,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let cleanup_deadline = import_cleanup_deadline(deadline)?;
    ensure_before_deadline(cleanup_deadline.monotonic())?;
    let scope = await_import_precommit_read_until(
        cleanup_deadline.monotonic(),
        capture_scope_for_request(conn, request),
    )
    .await
    .context("resolve capture workspace scope before abandoning import attempt")?;
    ensure_before_deadline(cleanup_deadline.monotonic())?;
    let txn = begin_import_cleanup_transaction_until(
        conn,
        cleanup_deadline,
        "begin expired import attempt cleanup",
    )
    .await?;
    let initial_fence = await_import_precommit_read_until(
        cleanup_deadline.monotonic(),
        scope.assert_workspace_fence_live(&txn),
    )
    .await;
    if let Err(error) = initial_fence {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before abandoning import attempt");
    }
    ensure_before_deadline(cleanup_deadline.monotonic())?;
    // The lease owner is minted per import run, so every reservation it holds
    // has this binary's coverage schema version; the shared statement needs
    // no schema predicate to release exactly this attempt's claims.
    coverage_gate::abandon_reserved_turn_claims_with_conn(
        &txn,
        &request.session_id,
        &lease.owner,
        "import",
        abandonment.now_ms,
    )
    .await
    .context("abandon expired import coverage reservation")?;
    ensure_before_deadline(cleanup_deadline.monotonic())?;
    let released_identity = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_import_identity
         SET state = ?, owner = NULL, lease_expires_at = NULL,
             attempt_id = NULL, attempt_checkpoint_id = NULL,
             fence_token = COALESCE(fence_token, 0) + 1,
             last_error_code = ?, updated_at = ?
         WHERE identity_id = ? AND owner = ? AND fence_token = ?
           AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
           AND workspace_id IS ? AND workspace_fence IS ?
           AND (agent_import_identity.workspace_id IS NULL OR EXISTS (
               SELECT 1 FROM workspace_record
               WHERE workspace_id = agent_import_identity.workspace_id
                 AND repo_id = agent_import_identity.repo_id
                 AND lease_fence = agent_import_identity.workspace_fence
                 AND state IN ('provisioning', 'active', 'releasing')
                 AND lease_owner IS NOT NULL
                 AND lease_expires_at > (unixepoch('now') * 1000)
           ))",
            [
                abandonment.identity_state.into(),
                abandonment.last_error_code.into(),
                abandonment.now_ms.into(),
                lease.identity_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence_token.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("release expired import identity lease")?
        .rows_affected()
        == 1;
    if released_identity && let Some((checkpoint_id, marker_generation)) = abandonment.marker_fence
    {
        if let Err(error) = ensure_before_deadline(cleanup_deadline.monotonic()) {
            txn.rollback().await.ok();
            return Err(error).context("deadline expired before clearing import attempt marker");
        }
        // A rejected append may have converted this marker into a durable
        // cleanup job. Never erase that ownership record here.
        ImportedCheckpointWriter::clear_uncommitted_prebound_attempt(
            &txn,
            &request.session_id,
            checkpoint_id,
            marker_generation,
        )
        .await
        .map_err(|error| anyhow!("clear uncommitted import attempt marker: {error}"))?;
    }
    if released_identity {
        if let Err(error) = ensure_before_deadline(cleanup_deadline.monotonic()) {
            txn.rollback().await.ok();
            return Err(error)
                .context("deadline expired before removing provisional import session");
        }
        CaptureCatalogStore::discard_unprogressed_import_session(
            &txn,
            &scope,
            &catalog_session_for_request(request)?,
        )
        .await
        .map_err(|error| {
            map_import_catalog_error("remove zero-progress provisional import session", error)
        })?;
    }
    commit_import_transaction_until(
        txn,
        &scope,
        cleanup_deadline,
        "commit expired import attempt cleanup",
    )
    .await
}

async fn abandon_import_attempt_after_error(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    marker_fence: Option<(&str, &str)>,
    identity_state: &str,
    original: anyhow::Error,
    deadline: CaptureCommitDeadline,
) -> anyhow::Error {
    match abandon_import_attempt(
        conn,
        request,
        lease,
        ImportAttemptAbandonment {
            marker_fence,
            identity_state,
            last_error_code: "LBR-AGENT-018",
            now_ms: Utc::now().timestamp_millis(),
        },
        deadline,
    )
    .await
    {
        Ok(()) => original,
        Err(_) => {
            tracing::error!(
                reason = "import_attempt_cleanup_failed",
                "failed to abandon an unsuccessful historical import attempt"
            );
            original.context(
                "the import also failed to release its recovery ownership; \
                 run `libra agent doctor --repair` before retrying or erasing this session",
            )
        }
    }
}

fn redacted_json(value: &serde_json::Value) -> Result<RedactedBytes> {
    let bytes = serde_json::to_vec_pretty(value).context("serialize safe import projection")?;
    Ok(Redactor::new_default().redact(&bytes).0)
}

fn redacted_json_line(value: &serde_json::Value) -> Result<RedactedBytes> {
    let mut bytes = serde_json::to_vec(value).context("serialize safe import JSONL record")?;
    bytes.push(b'\n');
    Ok(Redactor::new_default().redact(&bytes).0)
}

fn import_lifecycle_event_id(session_id: &str, logical_turn_key: &str) -> uuid::Uuid {
    let mut name = Vec::with_capacity(session_id.len() + logical_turn_key.len() + 1);
    name.extend_from_slice(session_id.as_bytes());
    name.push(0);
    name.extend_from_slice(logical_turn_key.as_bytes());
    uuid::Uuid::new_v5(&IMPORT_LIFECYCLE_EVENT_NAMESPACE, &name)
}

#[cfg(test)]
fn import_test_failpoint(name: &str) -> Result<()> {
    if TEST_IMPORT_FAILPOINT
        .try_with(|configured| configured.is_some_and(|configured| configured == name))
        .unwrap_or(false)
    {
        bail!("test-injected import failure at {name}");
    }
    Ok(())
}

#[cfg(test)]
fn preserving_crash_failpoint_active() -> bool {
    TEST_IMPORT_FAILPOINT
        .try_with(|configured| configured == &Some("after_bind"))
        .unwrap_or(false)
}

async fn current_parent_commit(
    conn: &DatabaseConnection,
    deadline: Instant,
) -> Result<Option<String>> {
    await_import_precommit_read_until(deadline, async {
        match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
            Ok(commit) => Ok(commit.map(|hash| hash.to_string())),
            Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                if detail.contains("HEAD reference is missing") =>
            {
                Ok(None)
            }
            Err(error) => Err(anyhow!(
                "failed to resolve HEAD for import checkpoint: {error}"
            )),
        }
    })
    .await
}

async fn committed_source_ordinals(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    deadline: Instant,
) -> Result<BTreeSet<usize>> {
    let rows = await_import_precommit_read_until(deadline, async {
        conn.query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT logical_turn_key FROM agent_coverage_claim
             WHERE session_id = ? AND coverage_schema_version = ?
               AND state = 'catalog_committed'",
            [
                request.session_id.clone().into(),
                super::observed_agents::COVERAGE_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("load committed source ordinals for import cursor")
    })
    .await?;
    let committed_keys = rows
        .into_iter()
        .map(|row| row.try_get_by::<String, _>("logical_turn_key"))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    Ok(request
        .turns
        .iter()
        .filter(|turn| committed_keys.contains(&turn.logical_turn_key))
        .map(|turn| turn.ordinal)
        .collect())
}

fn contiguous_next_ordinal(ordinals: &BTreeSet<usize>) -> Result<i64> {
    let mut next = 0usize;
    while ordinals.contains(&next) {
        next = next
            .checked_add(1)
            .context("turn ordinal cursor overflow")?;
    }
    i64::try_from(next).context("turn ordinal exceeds import cursor range")
}

async fn capture_imported_subagent_content(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    storage_root: &std::path::Path,
    request: &ImportRequest,
    discovery: &super::subagent_content::SubagentDiscovery,
    deadline: CaptureCommitDeadline,
) -> Result<super::subagent_content::SubagentCaptureSummary> {
    if request.agent_kind != AgentKind::ClaudeCode {
        return Ok(super::subagent_content::SubagentCaptureSummary::default());
    }
    if discovery.warning.is_some() {
        tracing::warn!(
            reason = "subagent_discovery_unavailable",
            "historical import subagent discovery is unavailable; parent import remains valid"
        );
    }
    let summary = super::subagent_content::capture_discovered_subagent_contents_with_scope(
        conn,
        scope,
        storage_root,
        &request.session_id,
        &discovery.sources,
        "import",
        Some(deadline),
    )
    .await?;
    tracing::info!(
        discovered = summary.discovered,
        checkpoints_written = summary.checkpoints_written,
        skipped_unchanged = summary.skipped_unchanged,
        skipped_inflight = summary.skipped_inflight,
        partial_sources = summary.partial_sources,
        "historical import subagent content attribution completed"
    );
    Ok(summary)
}

fn subagent_capture_progress(
    error: &anyhow::Error,
) -> super::subagent_content::SubagentCaptureSummary {
    error
        .downcast_ref::<super::subagent_content::SubagentCaptureProgressError>()
        .map(|progress| progress.summary().clone())
        .unwrap_or_default()
}

fn historical_import_is_partial(
    skipped_inflight: usize,
    conflicted: usize,
    discovery: &super::subagent_content::SubagentDiscovery,
) -> bool {
    skipped_inflight > 0 || conflicted > 0 || discovery.partial_source_count() > 0
}

/// Persist one prepared source with per-turn checkpoints and the shared
/// claim/ref/catalog/identity transaction.
pub async fn import_prepared(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
) -> Result<ImportSummary> {
    let monotonic_deadline = deadline.monotonic();
    let discovery = if request.agent_kind == AgentKind::ClaudeCode {
        let discovery_deadline =
            super::subagent_content::discovery_deadline_preserving_parent(monotonic_deadline)?;
        match super::subagent_content::discover_claude_subagent_contents_bounded(
            &request.working_dir,
            &request.provider_session_id,
            discovery_deadline,
            super::observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES,
            super::subagent_content::MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .await
        {
            Ok(discovery) => discovery,
            Err(error) => {
                match super::subagent_content::SubagentDiscovery::from_deadline_error(&error) {
                    Some(discovery) => discovery,
                    None => return Err(error),
                }
            }
        }
    } else {
        super::subagent_content::SubagentDiscovery::default()
    };
    import_prepared_with_subagent_discovery(conn, storage_root, request, deadline, discovery)
        .await
        .map(|detailed| detailed.summary)
}

/// Command-path variant whose child discovery was already charged to the
/// batch input budget before any persistence begins.
pub(crate) async fn import_prepared_with_subagent_discovery(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
) -> Result<DetailedImportSummary> {
    let monotonic_deadline = deadline.monotonic();
    // A prepared request can cross the private preparation-helper boundary or
    // be supplied by an embedding caller. Revalidate all durable time fields
    // before even resolving a capture scope, let alone acquiring an identity
    // lease or writing a catalog row.
    validate_persisted_import_timestamps(&request, Utc::now().timestamp())?;
    assert_v2_durable_import_request(&request)?;
    ensure_before_deadline(monotonic_deadline)?;
    let import_scope = capture_scope_for_request_until(conn, &request, monotonic_deadline).await?;
    let owner = format!("import:{}:{}", std::process::id(), uuid::Uuid::new_v4());
    let started_ms = Utc::now().timestamp_millis();
    let lease = acquire_identity(conn, &request, &owner, started_ms, deadline).await?;
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        let error = abandon_import_attempt_after_error(
            conn, &request, &lease, None, "failed", error, deadline,
        )
        .await;
        return Err(error);
    }

    let mut ordered_turns = request.turns.iter().collect::<Vec<_>>();
    ordered_turns.sort_by(|left, right| left.logical_turn_key.cmp(&right.logical_turn_key));
    let ordered_owned = ordered_turns
        .iter()
        .map(|turn| (*turn).clone())
        .collect::<Vec<_>>();
    let reservation = coverage_gate::reserve_import_turn_claims_until(
        conn,
        &import_scope,
        &request.session_id,
        &ordered_owned,
        &lease.owner,
        started_ms,
        deadline,
    )
    .await;
    let outcome = match reservation {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                "failed",
                error.context("reserve import coverage claims"),
                deadline,
            )
            .await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        let error = abandon_import_attempt_after_error(
            conn, &request, &lease, None, "failed", error, deadline,
        )
        .await;
        return Err(error);
    }
    // A discovery warning (for example, secure child discovery being
    // unavailable on this platform) does not prove that child evidence exists.
    // Only observed incomplete sources or claim conflicts make the import
    // partial; the parent transcript remains independently valid.
    let mut partial = historical_import_is_partial(
        outcome.skipped_inflight,
        outcome.conflicted,
        &subagent_discovery,
    );
    let defer_identity_for_subagents = !subagent_discovery.sources.is_empty();
    if outcome.reserved.is_empty() {
        let capture = capture_imported_subagent_content(
            conn,
            &import_scope,
            storage_root,
            &request,
            &subagent_discovery,
            deadline,
        )
        .await;
        let capture_summary = match capture {
            Ok(summary) => summary,
            Err(error) => {
                let child_progress = subagent_capture_progress(&error);
                let _ = abandon_import_attempt_after_error(
                    conn,
                    &request,
                    &lease,
                    None,
                    "partial",
                    error
                        .context("capture subagent content for already-covered historical session"),
                    deadline,
                )
                .await;
                return Err(ImportProgressError {
                    summary: ImportSummary {
                        session_id: request.session_id.clone(),
                        agent_kind: request.agent_kind.as_db_str().to_string(),
                        turns_seen: request.turns.len(),
                        checkpoints_written: 0,
                        skipped_covered: outcome.skipped_covered,
                        skipped_inflight: outcome.skipped_inflight,
                        conflicted: outcome.conflicted,
                        partial: true,
                    },
                    subagent_checkpoints_written: child_progress.checkpoints_written,
                    import_identity_id: lease.identity_id.clone(),
                    import_fence_token: lease.fence_token,
                }
                .into());
            }
        };
        // Projection repeats completeness checks under the capture deadline;
        // preserve a newly observed incomplete child as a partial import
        // rather than discarding its durable checkpoint after discovery.
        partial |= capture_summary.partial_sources > 0;
        let identity_state = if partial { "partial" } else { "committed" };
        if let Err(error) = finalize_noop_identity(
            conn,
            &request,
            &lease,
            identity_state,
            partial.then_some("LBR-AGENT-018"),
            Utc::now().timestamp_millis(),
            deadline,
        )
        .await
        {
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                identity_state,
                error,
                deadline,
            )
            .await;
            return Err(error);
        }
        return Ok(DetailedImportSummary::new(
            ImportSummary {
                session_id: request.session_id,
                agent_kind: request.agent_kind.as_db_str().to_string(),
                turns_seen: request.turns.len(),
                checkpoints_written: 0,
                skipped_covered: outcome.skipped_covered,
                skipped_inflight: outcome.skipped_inflight,
                conflicted: outcome.conflicted,
                partial,
            },
            capture_summary.checkpoints_written,
            &lease,
        ));
    }

    let parent_commit = match current_parent_commit(conn, monotonic_deadline).await {
        Ok(parent) => parent,
        Err(error) => {
            let error = abandon_import_attempt_after_error(
                conn, &request, &lease, None, "failed", error, deadline,
            )
            .await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        let error = abandon_import_attempt_after_error(
            conn, &request, &lease, None, "failed", error, deadline,
        )
        .await;
        return Err(error);
    }
    let mut committed_ordinals =
        match committed_source_ordinals(conn, &request, monotonic_deadline).await {
            Ok(ordinals) => ordinals,
            Err(error) => {
                let error = abandon_import_attempt_after_error(
                    conn, &request, &lease, None, "failed", error, deadline,
                )
                .await;
                return Err(error);
            }
        };
    let mut written = 0usize;
    // Reservation arbitration is keyed by logical id, which is not
    // necessarily ordinal order. Persist in source ordinal order so a
    // failure cannot advance the durable cursor past an earlier uncommitted
    // turn merely because its provider key sorts later.
    let mut reserved = outcome.reserved.iter().collect::<Vec<_>>();
    reserved.sort_by_key(|claim| {
        request
            .turns
            .iter()
            .find(|turn| turn.logical_turn_key == claim.logical_turn_key)
            .map(|turn| turn.ordinal)
            .unwrap_or(usize::MAX)
    });
    for (claim_index, claim) in reserved.iter().enumerate() {
        let mut bound_checkpoint_id = None;
        let mut bound_marker_generation = None;
        let turn_result: Result<()> = async {
            ensure_before_deadline(monotonic_deadline)?;
            let turn = request
                .turns
                .iter()
                .find(|turn| turn.logical_turn_key == claim.logical_turn_key)
                .context("reserved import claim has no normalized turn")?;
            let boundary = request
                .turn_boundaries
                .get(&turn.logical_turn_key)
                .context("reserved import claim has no turn chronology")?;
            let checkpoint_id = uuid::Uuid::new_v4().to_string();
            let now_ms = Utc::now().timestamp_millis();
            bound_checkpoint_id = Some(checkpoint_id.clone());
            let attempt_marker = bind_attempt(
                conn,
                &request,
                &lease,
                claim,
                &checkpoint_id,
                now_ms,
                deadline,
            )
            .await?;
            let marker_generation = attempt_marker
                .generation
                .clone()
                .context("new import marker has no writer generation")?;
            bound_marker_generation = Some(marker_generation.clone());
            let checkpoint_writer =
                ImportedCheckpointWriter::from_prebound_attempt(conn, storage_root, attempt_marker)
                    .map_err(|error| {
                        anyhow!("adopt prebound imported checkpoint attempt: {error}")
                    })?
                    .with_capture_scope(import_scope.clone());
            // The import adapter owns only the derived-turn contract. These
            // zero-cost witnesses keep it bound to the common checkpoint
            // port and concrete backend without exposing a generic request or
            // permitting this orchestration layer to construct the backend.
            let _: &dyn CheckpointStore = &checkpoint_writer;
            let _: std::marker::PhantomData<TracesCheckpointStore<'_>> =
                checkpoint_writer.backend_type();
            #[cfg(test)]
            import_test_failpoint("after_bind")?;
            ensure_before_deadline(monotonic_deadline)?;
            let projection = safe_turn_projection(request.agent_kind.as_db_str(), turn);
            let transcript = redacted_json_line(&projection)?;
            let metadata = redacted_json(&serde_json::json!({
            "schema_version": history::CHECKPOINT_METADATA_SCHEMA_VERSION,
            "checkpoint_id": checkpoint_id,
            "session_id": request.session_id,
            "agent_kind": request.agent_kind.as_db_str(),
            "model": "unknown",
            "scope": "committed",
            "provider_session_id": request.provider_session_id,
            "working_dir": request.working_dir,
            "created_at": boundary.ended_at,
            "turn_started_at": boundary.started_at,
            "turn_ended_at": boundary.ended_at,
            "redaction_report": request.redaction_report,
            "transcript_snapshot": request.transcript_snapshot.clone(),
            "import": {
                "source_kind": request.source_kind,
                "source_id": request.source_id,
                "logical_turn_key": turn.logical_turn_key,
                "ordinal": turn.ordinal,
            }
            }))?;
            let lifecycle_event = LifecycleEvent {
                kind: LifecycleEventKind::TurnEnd,
                session_id: request.provider_session_id.clone(),
                session_ref: None,
                prompt: None,
                model: None,
                source: Some(serde_json::json!({
                    "channel": "import",
                    "source_kind": request.source_kind,
                })),
                tool_name: None,
                tool_input: None,
                tool_response: None,
                assistant_message: None,
                timestamp: DateTime::<Utc>::from_timestamp(boundary.ended_at, 0)
                    .context("import lifecycle timestamp is out of range")?,
            };
            let lifecycle_context = CanonicalEventContext {
                agent_kind: request.agent_kind.as_db_str(),
                session_id: &request.session_id,
                provider_session_id: &request.provider_session_id,
                identity_scheme: LifecycleIdentityScheme::ImportUuidV5,
                provenance: serde_json::json!({
                    "channel": "import",
                    "logical_turn_key": turn.logical_turn_key,
                }),
            };
            let lifecycle = redacted_json_line(&lifecycle_event_canonical_json_with_identity(
                &lifecycle_event,
                &lifecycle_context,
                import_lifecycle_event_id(&request.session_id, &turn.logical_turn_key),
                turn.completeness == Completeness::Incomplete,
            ))?;
            let report = redacted_json(&request.redaction_report)?;
            let source_snapshot = request
                .transcript_snapshot
                .clone()
                .context("historical import has no complete authorized source snapshot")?;
            let checkpoint_payload = CheckpointRedactedPayload::from_derived_turn_projection(
                transcript,
                source_snapshot,
                metadata,
                lifecycle,
                report,
            )
            .map_err(|error| anyhow!("prepare imported turn checkpoint payload: {error}"))?;
            // Only imports with actual child sources defer identity completion.
            // The established no-child path retains its atomic parent
            // checkpoint+identity commit and post-commit deadline semantics.
            let final_turn =
                claim_index + 1 == reserved.len() && !partial && !defer_identity_for_subagents;
            committed_ordinals.insert(turn.ordinal);
            let next_ordinal = contiguous_next_ordinal(&committed_ordinals)?;
            let plan = LiveClaimCommitPlan {
                source_channel: "import",
                session_id: request.session_id.clone(),
                checkpoint_id: checkpoint_id.clone(),
                owner: lease.owner.clone(),
                parent_commit: parent_commit.clone(),
                created_at: boundary.ended_at,
                now_ms,
                claims: vec![(*claim).clone()],
                import_session: Some(import_session_commit(&request, &import_scope)?),
                import_identity: Some(ImportIdentityCommit {
                    identity_id: lease.identity_id.clone(),
                    observed_digest: request.content_digest.clone(),
                    owner: lease.owner.clone(),
                    fence_token: lease.fence_token,
                    next_ordinal,
                    final_turn,
                }),
                capture_scope: Some(import_scope.clone()),
            };
            let checkpoint_request = ImportedCheckpointWriteRequest::new(
                &checkpoint_id,
                &request.session_id,
                request.agent_kind.as_db_str(),
                parent_commit.as_deref(),
                &checkpoint_payload,
                &plan,
                deadline,
            )
            .map_err(|error| anyhow!("prepare imported checkpoint write request: {error}"))?;
            match checkpoint_writer
                .write_turn(checkpoint_request)
                .await
                .map_err(|error| anyhow!("write imported turn checkpoint: {error}"))?
            {
                CheckpointWriteOutcome::Written { .. }
                | CheckpointWriteOutcome::AlreadyExists { .. } => {}
                CheckpointWriteOutcome::TerminalReceiptAlreadyApplied => {
                    bail!(
                        "import checkpoint store observed a terminal receipt acknowledgement, which is invalid for historical import"
                    );
                }
                CheckpointWriteOutcome::AttemptInFlight { .. } => {
                    bail!(
                        "import checkpoint attempt is already in flight; retry after the existing writer finishes"
                    );
                }
                CheckpointWriteOutcome::PendingCleanup { .. } => {
                    // The ref CAS and its import/coverage companion already
                    // committed. Preserve the historical importer contract:
                    // account for the turn while retaining the durable marker
                    // for doctor/GC repair instead of replaying a second
                    // checkpoint write.
                    tracing::warn!(
                        reason = "import_checkpoint_cleanup_pending",
                        "imported checkpoint committed with durable cleanup pending"
                    );
                }
                CheckpointWriteOutcome::ConflictUnchanged { reason } => {
                    bail!("imported checkpoint store left the write unchanged: {reason:?}");
                }
            }
            // A durable outcome transfers marker cleanup ownership to the
            // checkpoint store. In particular, do not let a later import
            // finalizer erase a `PendingCleanup` marker after its ref/CAS
            // committed; doctor/GC needs that marker as repair evidence.
            bound_marker_generation = None;
            written += 1;
            #[cfg(test)]
            import_test_failpoint("after_catalog_commit")?;
            if claim_index + 1 < reserved.len() {
                ensure_before_deadline(monotonic_deadline)?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = turn_result {
            let terminal_state = if written == 0 { "failed" } else { "partial" };
            #[cfg(test)]
            let preserve_test_crash = preserving_crash_failpoint_active();
            #[cfg(not(test))]
            let preserve_test_crash = false;
            let error = if preserve_test_crash {
                error
            } else {
                abandon_import_attempt_after_error(
                    conn,
                    &request,
                    &lease,
                    bound_checkpoint_id
                        .as_deref()
                        .zip(bound_marker_generation.as_deref()),
                    terminal_state,
                    error,
                    deadline,
                )
                .await
            };
            if written == 0 {
                return Err(error);
            }
            let summary = ImportSummary {
                session_id: request.session_id.clone(),
                agent_kind: request.agent_kind.as_db_str().to_string(),
                turns_seen: request.turns.len(),
                checkpoints_written: written,
                skipped_covered: outcome.skipped_covered,
                skipped_inflight: outcome.skipped_inflight,
                conflicted: outcome.conflicted,
                partial: true,
            };
            return Err(ImportProgressError {
                summary,
                subagent_checkpoints_written: 0,
                import_identity_id: lease.identity_id.clone(),
                import_fence_token: lease.fence_token,
            }
            .into());
        }
    }
    let capture = capture_imported_subagent_content(
        conn,
        &import_scope,
        storage_root,
        &request,
        &subagent_discovery,
        deadline,
    )
    .await;
    let capture_summary = match capture {
        Ok(summary) => summary,
        Err(error) => {
            let child_progress = subagent_capture_progress(&error);
            let _ = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                "partial",
                error.context("capture imported subagent content"),
                deadline,
            )
            .await;
            return Err(ImportProgressError {
                summary: ImportSummary {
                    session_id: request.session_id.clone(),
                    agent_kind: request.agent_kind.as_db_str().to_string(),
                    turns_seen: request.turns.len(),
                    checkpoints_written: written,
                    skipped_covered: outcome.skipped_covered,
                    skipped_inflight: outcome.skipped_inflight,
                    conflicted: outcome.conflicted,
                    partial: true,
                },
                subagent_checkpoints_written: child_progress.checkpoints_written,
                import_identity_id: lease.identity_id.clone(),
                import_fence_token: lease.fence_token,
            }
            .into());
        }
    };
    // Projection repeats completeness checks under the capture deadline;
    // preserve a newly observed incomplete child as a partial import rather
    // than abandoning already durable parent and child checkpoints.
    partial |= capture_summary.partial_sources > 0;
    let identity_state = if partial { "partial" } else { "committed" };
    if (partial || defer_identity_for_subagents)
        && let Err(error) = finalize_noop_identity(
            conn,
            &request,
            &lease,
            identity_state,
            partial.then_some("LBR-AGENT-018"),
            Utc::now().timestamp_millis(),
            deadline,
        )
        .await
    {
        let _ = abandon_import_attempt_after_error(
            conn, &request, &lease, None, "partial", error, deadline,
        )
        .await;
        return Err(ImportProgressError {
            summary: ImportSummary {
                session_id: request.session_id.clone(),
                agent_kind: request.agent_kind.as_db_str().to_string(),
                turns_seen: request.turns.len(),
                checkpoints_written: written,
                skipped_covered: outcome.skipped_covered,
                skipped_inflight: outcome.skipped_inflight,
                conflicted: outcome.conflicted,
                partial: true,
            },
            subagent_checkpoints_written: capture_summary.checkpoints_written,
            import_identity_id: lease.identity_id.clone(),
            import_fence_token: lease.fence_token,
        }
        .into());
    }
    Ok(DetailedImportSummary::new(
        ImportSummary {
            session_id: request.session_id,
            agent_kind: request.agent_kind.as_db_str().to_string(),
            turns_seen: request.turns.len(),
            checkpoints_written: written,
            skipped_covered: outcome.skipped_covered,
            skipped_inflight: outcome.skipped_inflight,
            conflicted: outcome.conflicted,
            partial,
        },
        capture_summary.checkpoints_written,
        &lease,
    ))
}

/// Fast tombstone check used by the command before reading/exporting content.
pub async fn session_is_tombstoned(
    conn: &DatabaseConnection,
    kind: AgentKind,
    provider_session_id: &str,
) -> Result<bool> {
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [kind.as_db_str().into(), provider_session_id.into()],
        ))
        .await
        .context("check agent import tombstone")?
        .is_some())
}

/// Explicit, audited local restore of an erased provider identity.
pub async fn restore_tombstone(
    conn: &DatabaseConnection,
    kind: AgentKind,
    provider_session_id: &str,
) -> Result<bool> {
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .context("begin erased-session restore")?;
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT erased_session_id FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [kind.as_db_str().into(), provider_session_id.into()],
        ))
        .await
        .context("read erased-session tombstone")?;
    let Some(row) = row else {
        txn.rollback().await.ok();
        return Ok(false);
    };
    let erased_session_id: String = row.try_get_by("erased_session_id")?;
    let erasure_finished = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_session WHERE session_id = ?",
            [erased_session_id.clone().into()],
        ))
        .await
        .context("verify local erasure completed before restore")?
        .is_none();
    if !erasure_finished {
        txn.rollback().await.ok();
        bail!(
            "the erased session is still being pruned; wait for local erasure to finish before restoring it"
        );
    }
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO agent_audit_log (
            audit_id, timestamp, action, checkpoint_id, scope, justification, granted
         ) VALUES (?, ?, 'restore_erased_import', ?, 'session', ?, 1)",
        [
            uuid::Uuid::new_v4().to_string().into(),
            Utc::now().to_rfc3339().into(),
            erased_session_id.into(),
            "explicit --restore-erased confirmation".into(),
        ],
    ))
    .await
    .context("append erased-session restore audit")?;
    let deleted = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "DELETE FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [kind.as_db_str().into(), provider_session_id.into()],
        ))
        .await
        .context("remove erased-session tombstone")?;
    txn.commit()
        .await
        .context("commit erased-session restore")?;
    Ok(deleted.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_deadline_after(duration: Duration) -> CaptureCommitDeadline {
        let monotonic = Instant::now()
            .checked_add(duration)
            .expect("test import deadline fits monotonic clock");
        let sqlite_not_after_millis = Utc::now()
            .timestamp_millis()
            .checked_add(
                i64::try_from(duration.as_millis())
                    .expect("test import deadline fits SQLite millis"),
            )
            .expect("test import deadline fits SQLite millis");
        CaptureCommitDeadline::from_test_pair(monotonic, sqlite_not_after_millis)
    }

    #[test]
    fn unavailable_child_discovery_does_not_invalidate_parent_import() {
        let discovery = super::super::subagent_content::SubagentDiscovery {
            warning: Some("secure discovery unavailable".to_string()),
            ..super::super::subagent_content::SubagentDiscovery::default()
        };
        assert!(!historical_import_is_partial(0, 0, &discovery));
        assert!(historical_import_is_partial(1, 0, &discovery));
    }

    #[tokio::test]
    async fn import_failpoints_are_scoped_to_in_process_test_futures() {
        assert!(import_test_failpoint("after_bind").is_ok());
        assert!(
            !preserving_crash_failpoint_active(),
            "an unscoped test must not inherit a crash injection"
        );

        with_import_failpoint("after_bind", async {
            let error = import_test_failpoint("after_bind")
                .expect_err("the selected in-process failpoint must fire");
            assert!(format!("{error:#}").contains("after_bind"));
            assert!(
                preserving_crash_failpoint_active(),
                "only the bound-attempt failure preserves ownership for replay"
            );
            assert!(
                import_test_failpoint("after_catalog_commit").is_ok(),
                "a scoped failpoint must not affect another recovery stage"
            );
        })
        .await;

        assert!(import_test_failpoint("after_bind").is_ok());
        assert!(
            !preserving_crash_failpoint_active(),
            "the in-process fault must be cleared when its future completes"
        );
    }

    /// Every mutable import transaction makes one final SQLite authorization
    /// against the paired deadline and workspace fence. A pause immediately
    /// before that authorization must roll back the provisional
    /// session/identity, attempt/claim/marker, and lifecycle catalog update
    /// instead of committing a late V2-visible mutation.
    #[tokio::test]
    async fn final_fence_deadline_rolls_back_import_identity_attempt_and_lifecycle_mutations() {
        let dir = tempfile::tempdir().expect("create final-fence deadline import fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create final-fence deadline import database");
        let scope = CaptureScope {
            repo_id: "import-final-fence-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("import-final-fence-deadline-workspace".to_string()),
            workspace_fence: Some(59),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                       'active', 'import-final-fence-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live import workspace");
        let source_commitment = format!("source/hmac-v2/{}", "f".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-final-fence-deadline-provider".to_string(),
            session_id: build_ai_session_id(
                capture_provider_name(AgentKind::ClaudeCode),
                "import-final-fence-deadline-provider",
            ),
            source_kind: "file".to_string(),
            source_id: source_commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "import-final-fence-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: source_commitment,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let identity_id = identity_id(&request);
        let short_deadline = || commit_deadline_after(Duration::from_millis(20));

        let error = with_import_final_fence_delay(
            Duration::from_millis(80),
            acquire_identity(
                &conn,
                &request,
                "final-fence-deadline-owner",
                1,
                short_deadline(),
            ),
        )
        .await
        .expect_err("post-fence acquire must honor the elapsed deadline");
        assert!(
            matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::DeadlineExceeded)
            ),
            "post-fence acquire must surface DeadlineExceeded, got: {error:#}"
        );
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.clone().into()],
            ))
            .await
            .expect("read rolled-back identity")
            .is_none(),
            "late acquire must not publish an import identity"
        );
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read rolled-back session")
            .is_none(),
            "late acquire must not publish a provisional catalog session"
        );

        // The monotonic clock remains live here; rejection must come from the
        // final SQLite authorization rather than a caller-side timeout after
        // identity/session DML has already been staged in the transaction.
        let sqlite_expired = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = acquire_identity(
            &conn,
            &request,
            "final-authorization-wall-expired-owner",
            2,
            sqlite_expired,
        )
        .await
        .expect_err("SQLite final authorization must reject a wall-expired import lease");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.clone().into()],
            ))
            .await
            .expect("read wall-expired identity")
            .is_none(),
            "SQLite final authorization must roll back the staged identity"
        );
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read wall-expired session")
            .is_none(),
            "SQLite final authorization must roll back the staged catalog session"
        );

        let lease = acquire_identity(
            &conn,
            &request,
            "final-fence-deadline-owner",
            3,
            commit_deadline_after(Duration::from_secs(5)),
        )
        .await
        .expect("acquire baseline import lease");
        let turn = NormalizedTurn {
            logical_turn_key: "final-fence-deadline-turn".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: Vec::new(),
        };
        let claim = coverage_gate::reserve_import_turn_claims_until(
            &conn,
            &scope,
            &request.session_id,
            std::slice::from_ref(&turn),
            &lease.owner,
            2,
            commit_deadline_after(Duration::from_secs(5)),
        )
        .await
        .expect("reserve baseline import claim")
        .reserved
        .into_iter()
        .next()
        .expect("one reserved import claim");
        let checkpoint_id = "import-final-fence-deadline-checkpoint";

        let error = with_import_final_fence_delay(
            Duration::from_millis(80),
            bind_attempt(
                &conn,
                &request,
                &lease,
                &claim,
                checkpoint_id,
                3,
                short_deadline(),
            ),
        )
        .await
        .expect_err("post-fence bind must honor the elapsed deadline");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        let identity_after_bind = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_import_identity
                 WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after late bind")
            .expect("baseline identity remains");
        assert_eq!(
            identity_after_bind
                .try_get_by::<String, _>("state")
                .expect("decode identity state"),
            "leased"
        );
        assert_eq!(
            identity_after_bind
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            identity_after_bind
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode identity attempt"),
            None
        );
        let claim_after_bind = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    request.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read claim after late bind")
            .expect("baseline claim remains");
        assert_eq!(
            claim_after_bind
                .try_get_by::<String, _>("state")
                .expect("decode claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim_after_bind
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode claim attempt"),
            None
        );
        assert!(
            MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentTracesInflight,
                &request.session_id,
                checkpoint_id,
            )
            .await
            .expect("read marker after late bind")
            .is_none(),
            "late bind must not publish an in-flight marker"
        );

        let session_before_finalize = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, sync_revision FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read baseline catalog session")
            .expect("baseline catalog session exists");
        let session_before_finalize = (
            session_before_finalize
                .try_get_by::<String, _>("state")
                .expect("decode baseline catalog state"),
            session_before_finalize
                .try_get_by::<i64, _>("sync_revision")
                .expect("decode baseline catalog revision"),
        );
        let error = with_import_final_fence_delay(
            Duration::from_millis(80),
            finalize_noop_identity(
                &conn,
                &request,
                &lease,
                "committed",
                None,
                4,
                short_deadline(),
            ),
        )
        .await
        .expect_err("post-fence finalization must honor the elapsed deadline");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        let identity_after_finalize = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, lease_expires_at FROM agent_import_identity
                 WHERE identity_id = ?",
                [lease.identity_id.into()],
            ))
            .await
            .expect("read identity after late finalization")
            .expect("baseline identity remains after finalization rollback");
        assert_eq!(
            identity_after_finalize
                .try_get_by::<String, _>("state")
                .expect("decode final identity state"),
            "leased"
        );
        assert_eq!(
            identity_after_finalize
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode final identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        let session_after_finalize = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, sync_revision FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read catalog after late finalization")
            .expect("catalog session remains");
        let session_after_finalize = (
            session_after_finalize
                .try_get_by::<String, _>("state")
                .expect("decode final catalog state"),
            session_after_finalize
                .try_get_by::<i64, _>("sync_revision")
                .expect("decode final catalog revision"),
        );
        assert_eq!(
            session_after_finalize, session_before_finalize,
            "late finalization must not publish a catalog lifecycle update"
        );
    }

    /// The normal import deadline must bound a real file-backed SQLite writer
    /// wait before any session or identity DML starts. Releasing the holder
    /// must not let the cancelled acquisition publish delayed durable state.
    #[tokio::test]
    async fn import_identity_deadline_bounds_file_sqlite_writer_acquisition_without_delayed_rows() {
        let dir = tempfile::tempdir().expect("create import identity lock fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import identity lock database");
        let lock_conn = crate::internal::db::establish_connection(&db_path.to_string_lossy())
            .await
            .expect("open independent import identity lock connection");
        let source_commitment = format!("source/hmac-v2/{}", "d".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-identity-lock-provider".to_string(),
            session_id: build_ai_session_id(
                capture_provider_name(AgentKind::ClaudeCode),
                "import-identity-lock-provider",
            ),
            source_kind: "file".to_string(),
            source_id: source_commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "import-identity-lock-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_string(),
            capture_scope: Some(CaptureScope {
                repo_id: "import-identity-lock-repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            }),
            source_fingerprint: source_commitment,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let identity_id = identity_id(&request);
        let holder = crate::internal::db::begin_write_transaction(&lock_conn)
            .await
            .expect("hold file SQLite writer for import identity acquisition");
        let started = Instant::now();
        let error = acquire_identity(
            &conn,
            &request,
            "import-identity-lock-owner",
            1,
            commit_deadline_after(Duration::from_millis(30)),
        )
        .await
        .expect_err("contended import identity acquisition must observe its deadline");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "import identity writer acquisition ignored its short deadline"
        );
        holder
            .rollback()
            .await
            .expect("release file SQLite writer after import identity deadline");
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.into()],
            ))
            .await
            .expect("read import identity after cancelled acquisition")
            .is_none(),
            "a cancelled writer acquisition must not publish an import identity later"
        );
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read import session after cancelled acquisition")
            .is_none(),
            "a cancelled writer acquisition must not publish a catalog session later"
        );
    }

    struct LegacyImportMigrationFixture {
        _directory: tempfile::TempDir,
        conn: DatabaseConnection,
        repo_root: PathBuf,
        storage_root: PathBuf,
        scope: CaptureScope,
        provider_session_id: String,
        source_kind: String,
        legacy_source_id: String,
        v2_source_id: String,
        legacy_identity_id: String,
        v2_identity_id: String,
        session_id: String,
    }

    async fn seed_legacy_import_migration_fixture(
        identity_state: &str,
        marker_state: Option<(&str, i64)>,
    ) -> LegacyImportMigrationFixture {
        let directory = tempfile::tempdir().expect("create legacy migration fixture");
        let repo_root = directory.path().join("repo");
        let storage_root = repo_root.join(".libra");
        std::fs::create_dir_all(storage_root.join("objects"))
            .expect("create legacy migration object storage");
        let conn = crate::internal::db::create_database(
            &storage_root
                .join(crate::utils::util::DATABASE)
                .to_string_lossy(),
        )
        .await
        .expect("create legacy migration database");
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .expect("seed legacy migration repository identity");
        let scope = CaptureScope::resolve(&conn, &repo_root)
            .await
            .expect("resolve legacy migration capture scope");
        let agent_kind = AgentKind::ClaudeCode;
        let provider_session_id = "legacy-migration-provider".to_string();
        let source_kind = "file".to_string();
        let legacy_source_id = "project/session.jsonl".to_string();
        let v2_source_id = format!("source/hmac-v2/{}", "a".repeat(64));
        let legacy_identity_id = import_identity_id_for_parts(
            agent_kind,
            &provider_session_id,
            &source_kind,
            &legacy_source_id,
            IMPORT_IDENTITY_SCHEMA_VERSION_V1,
        );
        let v2_identity_id = import_identity_id_for_parts(
            agent_kind,
            &provider_session_id,
            &source_kind,
            &v2_source_id,
            IMPORT_IDENTITY_SCHEMA_VERSION_V2,
        );
        let session_id =
            build_ai_session_id(capture_provider_name(agent_kind), &provider_session_id);
        let legacy_fingerprint =
            legacy_import_source_fingerprint(agent_kind, &source_kind, &legacy_source_id);
        let legacy_repository_identity = legacy_import_repository_identity(&storage_root)
            .expect("derive legacy migration storage proof");
        let metadata = serde_json::json!({
            "repository_identity": legacy_repository_identity,
            "source_kind": source_kind,
            "source_id": legacy_source_id,
            "source_fingerprint": legacy_fingerprint,
            "import_source_schema_version": IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            "import_provisional": false,
            "imported": true,
        })
        .to_string();
        let redaction_report = serde_json::json!({
            "import": {
                "pipeline": "typed_allowlist",
                "snapshot_redaction": true,
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }
        })
        .to_string();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, stopped_at,
                schema_version, sync_revision, repo_id, worktree_id, workspace_id,
                workspace_fence, scope_state
             ) VALUES (?, ?, ?, 'stopped', ?, ?, ?, 1, 1, 1, 1, 1, ?, ?, ?, ?, 'scoped')",
            [
                session_id.clone().into(),
                agent_kind.as_db_str().into(),
                provider_session_id.clone().into(),
                repo_root.to_string_lossy().into_owned().into(),
                metadata.into(),
                redaction_report.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed committed legacy migration catalog session");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, observed_digest, committed_digest, attempt_id,
                attempt_checkpoint_id, next_ordinal, state, owner, lease_expires_at,
                fence_token, created_at, updated_at, repo_id, worktree_id, workspace_id,
                workspace_fence, scope_state
             ) VALUES (?, ?, ?, ?, ?, ?, 'digest', 'digest', NULL, NULL, 1, ?, NULL, NULL,
                       1, 1, 1, ?, ?, ?, ?, 'scoped')",
            [
                legacy_identity_id.clone().into(),
                agent_kind.as_db_str().into(),
                provider_session_id.clone().into(),
                source_kind.clone().into(),
                legacy_source_id.clone().into(),
                IMPORT_IDENTITY_SCHEMA_VERSION_V1.into(),
                identity_state.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed legacy migration identity");
        if let Some((state, lease_expires_at)) = marker_state {
            let marker = ImportIndexRepairMarker {
                schema_version: IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V1,
                owner: "legacy-migration-owner".to_string(),
                generation: "legacy-migration-generation".to_string(),
                identity_id: legacy_identity_id.clone(),
                agent_kind: agent_kind.as_db_str().to_string(),
                provider_session_id: provider_session_id.clone(),
                source_kind: source_kind.clone(),
                source_id: legacy_source_id.clone(),
                state: state.to_string(),
                lease_expires_at,
                created_at: 1,
                fence_token: None,
                capture_scope: Some(scope.clone()),
            };
            MetadataKv::set_with_conn(
                &conn,
                MetadataScope::AgentImportIndexRepair,
                &session_id,
                IMPORT_INDEX_REPAIR_MARKER_KEY,
                &serde_json::to_string(&marker).expect("encode legacy migration marker"),
                crate::internal::metadata::MetadataValueType::Text,
            )
            .await
            .expect("seed legacy migration marker");
        }
        LegacyImportMigrationFixture {
            _directory: directory,
            conn,
            repo_root,
            storage_root,
            scope,
            provider_session_id,
            source_kind,
            legacy_source_id,
            v2_source_id,
            legacy_identity_id,
            v2_identity_id,
            session_id,
        }
    }

    #[tokio::test]
    async fn scoped_committed_legacy_import_migrates_identity_catalog_and_expired_marker_together()
    {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        let migrated = migrate_legacy_import_ownership_in_scope_until(
            &fixture.conn,
            LegacyImportMigrationRequest {
                scope: &fixture.scope,
                storage_root: &fixture.storage_root,
                authorized_root: &fixture.repo_root,
                agent_kind: AgentKind::ClaudeCode,
                provider_session_id: &fixture.provider_session_id,
                source_kind: &fixture.source_kind,
                legacy_source_id: &fixture.legacy_source_id,
                v2_source_commitment: &fixture.v2_source_id,
                deadline: commit_deadline_after(std::time::Duration::from_secs(5)),
            },
        )
        .await
        .expect("migrate quiescent committed legacy import");
        assert!(migrated);
        let identity = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT identity_id, source_id, schema_version, state, owner, lease_expires_at
                 FROM agent_import_identity WHERE provider_session_id = ?",
                [fixture.provider_session_id.clone().into()],
            ))
            .await
            .expect("read migrated identity")
            .expect("migrated identity exists");
        assert_eq!(
            identity
                .try_get_by::<String, _>("identity_id")
                .expect("identity id"),
            fixture.v2_identity_id
        );
        assert_eq!(
            identity
                .try_get_by::<String, _>("source_id")
                .expect("source id"),
            fixture.v2_source_id
        );
        assert_eq!(
            identity
                .try_get_by::<i64, _>("schema_version")
                .expect("schema"),
            2
        );
        assert_eq!(
            identity.try_get_by::<String, _>("state").expect("state"),
            "committed"
        );
        assert!(
            identity
                .try_get_by::<Option<String>, _>("owner")
                .expect("owner")
                .is_none()
        );
        assert!(
            identity
                .try_get_by::<Option<i64>, _>("lease_expires_at")
                .expect("lease")
                .is_none()
        );
        let metadata: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read migrated catalog")
            .expect("migrated catalog exists")
            .try_get_by("metadata_json")
            .expect("catalog metadata");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("valid catalog metadata");
        assert_eq!(metadata["source_id"], fixture.v2_source_id);
        assert_eq!(metadata["source_fingerprint"], fixture.v2_source_id);
        assert_eq!(
            metadata["repository_identity"],
            IMPORT_DURABLE_IDENTITY_NOT_RETAINED
        );
        assert_eq!(metadata["import_source_schema_version"], 2);
        assert!(
            !metadata.to_string().contains(&fixture.legacy_source_id),
            "migrated mutable catalog metadata must not retain the legacy locator"
        );
        let marker = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read migrated repair marker")
        .expect("expired repair marker remains associated");
        let marker: ImportIndexRepairMarker =
            serde_json::from_str(&marker.value).expect("decode migrated repair marker");
        assert_eq!(marker.schema_version, IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2);
        assert_eq!(marker.identity_id, fixture.v2_identity_id);
        assert_eq!(marker.source_id, fixture.v2_source_id);
        assert_eq!(marker.capture_scope.as_ref(), Some(&fixture.scope));
        let legacy = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT 1 FROM agent_import_identity WHERE identity_id = ?",
                [fixture.legacy_identity_id.clone().into()],
            ))
            .await
            .expect("check retired legacy identity");
        assert!(
            legacy.is_none(),
            "the mutable V1 identity must be replaced, not duplicated"
        );
    }

    #[test]
    fn v2_existing_session_ownership_rejects_a_source_commitment_swap() {
        let directory = tempfile::tempdir().expect("create V2 ownership repository");
        let repository = directory.path().join("repo");
        let nested = repository.join("nested/work");
        std::fs::create_dir_all(repository.join(".libra")).expect("create V2 ownership storage");
        std::fs::create_dir_all(&nested).expect("create V2 ownership subdirectory");
        std::fs::File::create(repository.join(".libra/libra.db"))
            .expect("create V2 ownership database marker");
        let repository = repository
            .canonicalize()
            .expect("canonical V2 ownership root");
        let nested = nested
            .canonicalize()
            .expect("canonical V2 ownership subdirectory");
        let source_id = format!("source/hmac-v2/{}", "b".repeat(64));
        let mut request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "v2-source-swap-provider".to_string(),
            session_id: build_ai_session_id(
                capture_provider_name(AgentKind::ClaudeCode),
                "v2-source-swap-provider",
            ),
            source_kind: "file".to_string(),
            source_id: source_id.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "v2-source-swap-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "stopped".to_string(),
            stopped_at: Some(1),
            working_dir: repository.clone(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_string(),
            capture_scope: None,
            source_fingerprint: source_id.clone(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({}),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let snapshot = ExistingSessionOwnershipSnapshot {
            session_id: request.session_id.clone(),
            agent_kind: request.agent_kind.as_db_str().to_string(),
            provider_session_id: request.provider_session_id.clone(),
            working_dir: nested.to_string_lossy().into_owned(),
            metadata_json: serde_json::json!({
                "repository_identity": IMPORT_DURABLE_IDENTITY_NOT_RETAINED,
                "source_kind": "file",
                "source_id": source_id.clone(),
                "source_fingerprint": source_id,
                "import_source_schema_version": IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            })
            .to_string(),
        };
        validate_scoped_prepared_existing_session(&mut request, Some(&snapshot))
            .expect("exact V2 scoped session ownership should validate");
        assert!(request.existing_session_fingerprint.is_some());
        assert_eq!(request.working_dir, nested);

        let unavailable_catalog_path = directory.path().join("live-subdirectory-no-longer-mounted");
        let unavailable_path_snapshot = ExistingSessionOwnershipSnapshot {
            working_dir: unavailable_catalog_path.to_string_lossy().into_owned(),
            ..snapshot.clone()
        };
        validate_scoped_prepared_existing_session(&mut request, Some(&unavailable_path_snapshot))
            .expect("scoped catalog path is retained without parent filesystem access");
        assert_eq!(request.working_dir, unavailable_catalog_path);

        let noncanonical_snapshot = ExistingSessionOwnershipSnapshot {
            working_dir: format!("{}/../escape", directory.path().display()),
            ..snapshot.clone()
        };
        let error =
            validate_scoped_prepared_existing_session(&mut request, Some(&noncanonical_snapshot))
                .expect_err("scoped catalog path must remain lexically canonical");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));

        for working_dir in [
            format!("{}/", nested.display()),
            format!("{}//nested/work", repository.display()),
        ] {
            let noncanonical_snapshot = ExistingSessionOwnershipSnapshot {
                working_dir,
                ..snapshot.clone()
            };
            let error = validate_scoped_prepared_existing_session(
                &mut request,
                Some(&noncanonical_snapshot),
            )
            .expect_err("scoped catalog path must not have separator aliases");
            assert!(matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::RepositoryConflict)
            ));
        }

        let live_snapshot = ExistingSessionOwnershipSnapshot {
            metadata_json: "{}".to_string(),
            ..snapshot.clone()
        };
        validate_scoped_prepared_existing_session(&mut request, Some(&live_snapshot))
            .expect("a scoped live session without import metadata may be joined by V2 import");

        let partial_snapshot = ExistingSessionOwnershipSnapshot {
            metadata_json: serde_json::json!({"source_id": request.source_id.clone()}).to_string(),
            ..snapshot.clone()
        };
        let error =
            validate_scoped_prepared_existing_session(&mut request, Some(&partial_snapshot))
                .expect_err("partial import metadata must not be treated as a live session");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));

        request.source_id = format!("source/hmac-v2/{}", "c".repeat(64));
        request.source_fingerprint = request.source_id.clone();
        let error = validate_scoped_prepared_existing_session(&mut request, Some(&snapshot))
            .expect_err("a swapped V2 source commitment must be rejected");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
    }

    async fn migrate_legacy_fixture(fixture: &LegacyImportMigrationFixture) -> Result<bool> {
        migrate_legacy_import_ownership_in_scope_until(
            &fixture.conn,
            LegacyImportMigrationRequest {
                scope: &fixture.scope,
                storage_root: &fixture.storage_root,
                authorized_root: &fixture.repo_root,
                agent_kind: AgentKind::ClaudeCode,
                provider_session_id: &fixture.provider_session_id,
                source_kind: &fixture.source_kind,
                legacy_source_id: &fixture.legacy_source_id,
                v2_source_commitment: &fixture.v2_source_id,
                deadline: commit_deadline_after(std::time::Duration::from_secs(5)),
            },
        )
        .await
    }

    async fn assert_legacy_migration_rejected_and_retained(
        fixture: &LegacyImportMigrationFixture,
        v2_already_existed: bool,
    ) {
        let error = migrate_legacy_fixture(fixture)
            .await
            .expect_err("unsafe legacy state must not mint a parallel V2 identity");
        assert!(
            error.downcast_ref::<ImportError>().is_some_and(|error| {
                matches!(
                    error,
                    ImportError::RepositoryConflict | ImportError::LeaseBusy
                )
            }),
            "unsafe legacy state must fail with a stable import error: {error:#}"
        );
        let identity = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT identity_id, source_id, schema_version FROM agent_import_identity
                 WHERE identity_id = ?",
                [fixture.legacy_identity_id.clone().into()],
            ))
            .await
            .expect("read retained legacy identity")
            .expect("legacy identity remains");
        assert_eq!(
            identity
                .try_get_by::<String, _>("identity_id")
                .expect("identity id"),
            fixture.legacy_identity_id
        );
        assert_eq!(
            identity
                .try_get_by::<String, _>("source_id")
                .expect("legacy source id"),
            fixture.legacy_source_id
        );
        assert_eq!(
            identity
                .try_get_by::<i64, _>("schema_version")
                .expect("legacy schema"),
            1
        );
        let metadata: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read retained legacy catalog")
            .expect("legacy catalog remains")
            .try_get_by("metadata_json")
            .expect("legacy catalog metadata");
        assert!(
            metadata.contains(&fixture.legacy_source_id),
            "a rejected migration must leave its V1 proof intact for recovery"
        );
        assert!(!metadata.contains(&fixture.v2_source_id));
        let v2 = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT 1 FROM agent_import_identity WHERE identity_id = ?",
                [fixture.v2_identity_id.clone().into()],
            ))
            .await
            .expect("check that rejected migration did not mint V2 identity");
        assert_eq!(
            v2.is_some(),
            v2_already_existed,
            "unsafe legacy state must not create a new V2 identity"
        );
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_partial_or_live_identity_without_creating_v2() {
        let partial = seed_legacy_import_migration_fixture("partial", None).await;
        assert_legacy_migration_rejected_and_retained(&partial, false).await;

        let leased = seed_legacy_import_migration_fixture("leased", None).await;
        leased
            .conn
            .execute_raw(Statement::from_sql_and_values(
                leased.conn.get_database_backend(),
                "UPDATE agent_import_identity SET owner = 'live-owner', lease_expires_at = ?
                 WHERE identity_id = ?",
                [
                    Utc::now().timestamp_millis().saturating_add(60_000).into(),
                    leased.legacy_identity_id.clone().into(),
                ],
            ))
            .await
            .expect("make legacy lease live");
        assert_legacy_migration_rejected_and_retained(&leased, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_a_dangling_terminal_checkpoint_binding() {
        let fixture = seed_legacy_import_migration_fixture("committed", None).await;
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE agent_import_identity
                 SET attempt_id = 'missing-terminal-checkpoint',
                     attempt_checkpoint_id = 'missing-terminal-checkpoint'
                 WHERE identity_id = ?",
                [fixture.legacy_identity_id.clone().into()],
            ))
            .await
            .expect("seed dangling legacy terminal checkpoint binding");
        assert_legacy_migration_rejected_and_retained(&fixture, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_a_second_v1_locator_for_the_same_provider_tuple() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        let other_source = "project/swapped-session.jsonl";
        let other_identity = import_identity_id_for_parts(
            AgentKind::ClaudeCode,
            &fixture.provider_session_id,
            &fixture.source_kind,
            other_source,
            IMPORT_IDENTITY_SCHEMA_VERSION_V1,
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO agent_import_identity (
                    identity_id, agent_kind, provider_session_id, source_kind, source_id,
                    schema_version, next_ordinal, state, owner, lease_expires_at,
                    fence_token, created_at, updated_at, repo_id, worktree_id,
                    workspace_id, workspace_fence, scope_state
                 ) VALUES (?, 'claude_code', ?, ?, ?, 1, 0, 'committed', NULL, NULL,
                           1, 1, 1, ?, ?, ?, ?, 'scoped')",
                [
                    other_identity.into(),
                    fixture.provider_session_id.clone().into(),
                    fixture.source_kind.clone().into(),
                    other_source.into(),
                    fixture.scope.repo_id.clone().into(),
                    fixture.scope.worktree_id.clone().into(),
                    fixture.scope.workspace_id.clone().into(),
                    fixture.scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed conflicting legacy locator");
        assert_legacy_migration_rejected_and_retained(&fixture, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_a_v1_proof_with_a_different_source_kind() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        let other_source_kind = "export";
        let other_identity = import_identity_id_for_parts(
            AgentKind::ClaudeCode,
            &fixture.provider_session_id,
            other_source_kind,
            &fixture.legacy_source_id,
            IMPORT_IDENTITY_SCHEMA_VERSION_V1,
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO agent_import_identity (
                    identity_id, agent_kind, provider_session_id, source_kind, source_id,
                    schema_version, next_ordinal, state, owner, lease_expires_at,
                    fence_token, created_at, updated_at, repo_id, worktree_id,
                    workspace_id, workspace_fence, scope_state
                 ) VALUES (?, 'claude_code', ?, ?, ?, 1, 0, 'committed', NULL, NULL,
                           1, 1, 1, ?, ?, ?, ?, 'scoped')",
                [
                    other_identity.into(),
                    fixture.provider_session_id.clone().into(),
                    other_source_kind.into(),
                    fixture.legacy_source_id.clone().into(),
                    fixture.scope.repo_id.clone().into(),
                    fixture.scope.worktree_id.clone().into(),
                    fixture.scope.workspace_id.clone().into(),
                    fixture.scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed conflicting legacy source kind");
        assert_legacy_migration_rejected_and_retained(&fixture, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_a_different_v2_commitment_without_touching_v1_state() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        let conflicting_source_id = format!("source/hmac-v2/{}", "b".repeat(64));
        let conflicting_identity_id = import_identity_id_for_parts(
            AgentKind::ClaudeCode,
            &fixture.provider_session_id,
            &fixture.source_kind,
            &conflicting_source_id,
            IMPORT_IDENTITY_SCHEMA_VERSION_V2,
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO agent_import_identity (
                    identity_id, agent_kind, provider_session_id, source_kind, source_id,
                    schema_version, next_ordinal, state, owner, lease_expires_at,
                    fence_token, created_at, updated_at, repo_id, worktree_id,
                    workspace_id, workspace_fence, scope_state
                 ) VALUES (?, 'claude_code', ?, ?, ?, 2, 0, 'committed', NULL, NULL,
                           1, 1, 1, ?, ?, ?, ?, 'scoped')",
                [
                    conflicting_identity_id.into(),
                    fixture.provider_session_id.clone().into(),
                    fixture.source_kind.clone().into(),
                    conflicting_source_id.clone().into(),
                    fixture.scope.repo_id.clone().into(),
                    fixture.scope.worktree_id.clone().into(),
                    fixture.scope.workspace_id.clone().into(),
                    fixture.scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed conflicting V2 commitment");
        let metadata_before: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read legacy catalog before V2 collision")
            .expect("legacy catalog exists")
            .try_get_by("metadata_json")
            .expect("decode legacy metadata");
        let marker_before = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read legacy marker before V2 collision")
        .expect("legacy marker exists")
        .value;

        let error = migrate_legacy_fixture(&fixture)
            .await
            .expect_err("a different V2 commitment must block legacy migration");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));

        let v1 = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT source_id, schema_version FROM agent_import_identity WHERE identity_id = ?",
                [fixture.legacy_identity_id.clone().into()],
            ))
            .await
            .expect("read retained V1 identity")
            .expect("V1 identity remains");
        assert_eq!(
            v1.try_get_by::<String, _>("source_id")
                .expect("decode retained V1 source"),
            fixture.legacy_source_id
        );
        assert_eq!(
            v1.try_get_by::<i64, _>("schema_version")
                .expect("decode retained V1 schema"),
            IMPORT_IDENTITY_SCHEMA_VERSION_V1
        );
        let metadata_after: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read catalog after rejected V2 collision")
            .expect("legacy catalog remains")
            .try_get_by("metadata_json")
            .expect("decode retained catalog metadata");
        assert_eq!(metadata_after, metadata_before);
        let marker_after = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read marker after rejected V2 collision")
        .expect("legacy marker remains")
        .value;
        assert_eq!(marker_after, marker_before);
        let v2_count: i64 = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT COUNT(*) AS count FROM agent_import_identity
                 WHERE agent_kind = ? AND provider_session_id = ? AND schema_version = 2",
                [
                    AgentKind::ClaudeCode.as_db_str().into(),
                    fixture.provider_session_id.clone().into(),
                ],
            ))
            .await
            .expect("count V2 identities")
            .expect("V2 count row")
            .try_get_by("count")
            .expect("decode V2 identity count");
        assert_eq!(v2_count, 1, "migration must not mint a second V2 namespace");
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_raw_catalog_extra_without_relabelling_v1() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE agent_session
                 SET metadata_json = json_set(metadata_json, '$.transcript_path', ?)
                 WHERE session_id = ?",
                [
                    "/private/provider/session.jsonl".into(),
                    fixture.session_id.clone().into(),
                ],
            ))
            .await
            .expect("seed raw legacy catalog extra");
        let metadata_before: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read tainted legacy catalog")
            .expect("legacy catalog exists")
            .try_get_by("metadata_json")
            .expect("decode tainted legacy catalog");
        let marker_before = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read legacy marker before unsafe catalog migration")
        .expect("legacy marker exists")
        .value;

        let error = migrate_legacy_fixture(&fixture)
            .await
            .expect_err("raw V1 catalog metadata must not be relabelled as V2");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
        let metadata_after: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read catalog after rejected unsafe migration")
            .expect("legacy catalog remains")
            .try_get_by("metadata_json")
            .expect("decode retained legacy catalog");
        assert_eq!(
            metadata_after, metadata_before,
            "rejected migration must leave the V1 catalog byte-identical"
        );
        let marker_after = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read marker after rejected unsafe migration")
        .expect("legacy marker remains")
        .value;
        assert_eq!(marker_after, marker_before);
        let v1 = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT source_id, schema_version FROM agent_import_identity WHERE identity_id = ?",
                [fixture.legacy_identity_id.clone().into()],
            ))
            .await
            .expect("read retained V1 identity")
            .expect("V1 identity remains");
        assert_eq!(
            v1.try_get_by::<String, _>("source_id")
                .expect("decode V1 source"),
            fixture.legacy_source_id
        );
        assert_eq!(
            v1.try_get_by::<i64, _>("schema_version")
                .expect("decode V1 schema"),
            IMPORT_IDENTITY_SCHEMA_VERSION_V1
        );
        let v2_count: i64 = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT COUNT(*) AS count FROM agent_import_identity
                 WHERE agent_kind = ? AND provider_session_id = ? AND schema_version = 2",
                [
                    AgentKind::ClaudeCode.as_db_str().into(),
                    fixture.provider_session_id.clone().into(),
                ],
            ))
            .await
            .expect("count V2 identities after rejected catalog migration")
            .expect("V2 count row")
            .try_get_by("count")
            .expect("decode V2 count");
        assert_eq!(v2_count, 0);
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_uncanonical_redaction_report_without_relabelling_v1() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE agent_session SET redaction_report = '{}' WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("seed legacy noncanonical redaction report");
        let before = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json, redaction_report FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read legacy catalog before redaction migration")
            .expect("legacy catalog exists");
        let metadata_before: String = before
            .try_get_by("metadata_json")
            .expect("legacy metadata before migration");
        let redaction_before: String = before
            .try_get_by("redaction_report")
            .expect("legacy redaction report before migration");
        let marker_before = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read legacy marker before redaction migration")
        .expect("legacy marker exists")
        .value;

        let error = migrate_legacy_fixture(&fixture)
            .await
            .expect_err("V1 redaction evidence must not be fabricated as V2 provenance");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
        let after = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json, redaction_report FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read retained V1 catalog")
            .expect("legacy catalog remains");
        assert_eq!(
            after
                .try_get_by::<String, _>("metadata_json")
                .expect("retained legacy metadata"),
            metadata_before,
            "a rejected migration must retain the V1 ownership proof byte-for-byte"
        );
        assert_eq!(
            after
                .try_get_by::<String, _>("redaction_report")
                .expect("retained legacy redaction report"),
            redaction_before,
            "a rejected migration must retain historical redaction evidence instead of rewriting it"
        );
        let marker_after = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read retained marker")
        .expect("legacy marker remains")
        .value;
        assert_eq!(marker_after, marker_before);
        assert_legacy_migration_rejected_and_retained(&fixture, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_bare_receipt_digest_without_relabelling_v1() {
        let fixture = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        let event_id = "00000000-0000-0000-0000-00000000cafe";
        let action_key = format!("capture-lifecycle-v1:{event_id}");
        let metadata: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read legacy catalog before receipt injection")
            .expect("legacy catalog exists")
            .try_get_by("metadata_json")
            .expect("legacy metadata");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("decode legacy metadata");
        metadata["capture_catalog_receipts_v1"] = serde_json::json!({
            "version": 1,
            "entries": [{
                "receipt_key": format!("capture-action-v1:{event_id}"),
                "event_id": event_id,
                "action_key": action_key,
                "intent": {
                    "phase": "stopped",
                    "stopped_at": { "set": { "timestamp": 1 } },
                    "checkpoint": "committed",
                },
                "status": "pending",
                "recorded_at": 1,
                "reserved_revision": 1,
                "finalizer": {
                    "version": 1,
                    "replay_key": format!("capture-lifecycle-v1:{event_id}"),
                    "marker_generation": "00000000-0000-0000-0000-00000000beef",
                    "source_digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    "deadline_millis": null,
                    "mode": "deferrable",
                    "first_attempt_millis": 1,
                    "attempts": 1,
                    "stage": "snapshot",
                    "status": "pending",
                    "quarantine_reason": null,
                }
            }]
        });
        let metadata_before = metadata.to_string();
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE agent_session SET metadata_json = ? WHERE session_id = ?",
                [
                    metadata_before.clone().into(),
                    fixture.session_id.clone().into(),
                ],
            ))
            .await
            .expect("seed bare legacy receipt digest");
        let marker_before = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read legacy marker before receipt migration")
        .expect("legacy marker exists")
        .value;

        let error = migrate_legacy_fixture(&fixture)
            .await
            .expect_err("a bare legacy receipt digest cannot enter V2 metadata");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
        let metadata_after: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [fixture.session_id.clone().into()],
            ))
            .await
            .expect("read retained legacy catalog")
            .expect("legacy catalog remains")
            .try_get_by("metadata_json")
            .expect("retained legacy metadata");
        assert_eq!(metadata_after, metadata_before);
        let marker_after = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentImportIndexRepair,
            &fixture.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read retained marker")
        .expect("legacy marker remains")
        .value;
        assert_eq!(marker_after, marker_before);
        assert_legacy_migration_rejected_and_retained(&fixture, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_refuses_live_repair_pending_or_malformed_marker() {
        let live_marker = seed_legacy_import_migration_fixture(
            "committed",
            Some((
                "active",
                Utc::now().timestamp_millis().saturating_add(60_000),
            )),
        )
        .await;
        assert_legacy_migration_rejected_and_retained(&live_marker, false).await;

        let repair_pending =
            seed_legacy_import_migration_fixture("committed", Some(("repair_pending", 0))).await;
        assert_legacy_migration_rejected_and_retained(&repair_pending, false).await;

        let malformed = seed_legacy_import_migration_fixture("committed", None).await;
        MetadataKv::set_with_conn(
            &malformed.conn,
            MetadataScope::AgentImportIndexRepair,
            &malformed.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
            r#"{"schema_version":1,"state":"active"}"#,
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("seed malformed legacy marker");
        assert_legacy_migration_rejected_and_retained(&malformed, false).await;
    }

    #[tokio::test]
    async fn legacy_import_migration_collision_or_identity_write_failure_rolls_back_catalog_and_marker()
     {
        let collision =
            seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        collision
            .conn
            .execute_raw(Statement::from_sql_and_values(
                collision.conn.get_database_backend(),
                "INSERT INTO agent_import_identity (
                    identity_id, agent_kind, provider_session_id, source_kind, source_id,
                    schema_version, next_ordinal, state, owner, lease_expires_at,
                    fence_token, created_at, updated_at, repo_id, worktree_id,
                    workspace_id, workspace_fence, scope_state
                 ) VALUES (?, 'claude_code', ?, ?, ?, 2, 0, 'committed', NULL, NULL,
                           1, 1, 1, ?, ?, ?, ?, 'scoped')",
                [
                    collision.v2_identity_id.clone().into(),
                    collision.provider_session_id.clone().into(),
                    collision.source_kind.clone().into(),
                    collision.v2_source_id.clone().into(),
                    collision.scope.repo_id.clone().into(),
                    collision.scope.worktree_id.clone().into(),
                    collision.scope.workspace_id.clone().into(),
                    collision.scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed V2 collision");
        assert_legacy_migration_rejected_and_retained(&collision, true).await;

        let rollback = seed_legacy_import_migration_fixture("committed", Some(("active", 0))).await;
        rollback
            .conn
            .execute_unprepared(&format!(
                "CREATE TRIGGER reject_v2_import_identity_migration
                 BEFORE UPDATE ON agent_import_identity
                 WHEN NEW.schema_version = 2
                 BEGIN
                     SELECT RAISE(ABORT, 'legacy={} v2={}');
                 END",
                rollback.legacy_source_id, rollback.v2_source_id,
            ))
            .await
            .expect("install migration rollback trigger");
        let error = migrate_legacy_fixture(&rollback)
            .await
            .expect_err("raw trigger text must be reduced to a typed migration conflict");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
        let rendered = format!("{error:#}");
        assert!(
            !rendered.contains(&rollback.legacy_source_id)
                && !rendered.contains(&rollback.v2_source_id),
            "migration errors must not surface legacy or V2 source identifiers: {rendered}"
        );
        assert_legacy_migration_rejected_and_retained(&rollback, false).await;
        let marker = MetadataKv::get_with_conn(
            &rollback.conn,
            MetadataScope::AgentImportIndexRepair,
            &rollback.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read rolled-back marker")
        .expect("legacy marker remains after failed identity update");
        let marker: ImportIndexRepairMarker =
            serde_json::from_str(&marker.value).expect("decode retained legacy marker");
        assert_eq!(marker.schema_version, IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V1);
        assert_eq!(marker.identity_id, rollback.legacy_identity_id);
        assert_eq!(marker.source_id, rollback.legacy_source_id);
    }

    /// An importer may reserve coverage while its task workspace is current
    /// and lose that lease before its error path runs. Neither a later import
    /// reservation nor cleanup may alter the old workspace's claim or lease.
    #[tokio::test]
    async fn expired_workspace_scope_blocks_import_reserve_and_abandon_mutations() {
        let dir = tempfile::tempdir().expect("create scoped import fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create scoped import database");
        let scope = CaptureScope {
            repo_id: "import-scope-test".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("import-scope-workspace".to_string()),
            workspace_fence: Some(13),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/import-scope-workspace', 'agent',
                       'active', 'import-scope-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live import workspace");

        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "scope-expiry-provider".to_string(),
            session_id: "claude_code__scope_expiry_import".to_string(),
            source_kind: "file".to_string(),
            source_id: "scope-expiry-source".to_string(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            content_digest: "scope-expiry-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "scope-expiry-repository".to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: "scope-expiry-source-fingerprint".to_string(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, 'active', ?, '{}', '{}', 1, 1, 1,
                       ?, ?, ?, ?, 'scoped')",
            [
                request.session_id.clone().into(),
                request.provider_session_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed scoped import session");

        let reserved_turn = NormalizedTurn {
            logical_turn_key: "scope-expiry-reserved".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: Vec::new(),
        };
        let lease = ImportLease {
            identity_id: "scope-expiry-identity".to_string(),
            owner: "scope-expiry-import-owner".to_string(),
            fence_token: 1,
        };
        let reserved = coverage_gate::reserve_import_turn_claims_until(
            &conn,
            &scope,
            &request.session_id,
            std::slice::from_ref(&reserved_turn),
            &lease.owner,
            1,
            commit_deadline_after(std::time::Duration::from_secs(1)),
        )
        .await
        .expect("reserve import claim while workspace lease is live");
        assert_eq!(reserved.reserved.len(), 1);
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, observed_digest, next_ordinal, state, owner,
                lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, ?, ?, 1, ?, 0, 'leased', ?,
                       9999999999999, ?, 1, 1, ?, ?, ?, ?, 'scoped')",
            [
                lease.identity_id.clone().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                request.content_digest.clone().into(),
                lease.owner.clone().into(),
                lease.fence_token.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed owned import identity");

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0
             WHERE workspace_id = 'import-scope-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after import reservation");

        let after_expiry_turn = NormalizedTurn {
            logical_turn_key: "scope-expiry-after-expiry".to_string(),
            ordinal: 1,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: Vec::new(),
        };
        let error = coverage_gate::reserve_import_turn_claims_until(
            &conn,
            &scope,
            &request.session_id,
            std::slice::from_ref(&after_expiry_turn),
            &lease.owner,
            2,
            commit_deadline_after(std::time::Duration::from_secs(1)),
        )
        .await
        .expect_err("expired workspace scope must not reserve another import claim");
        assert!(format!("{error:#}").contains("workspace lease"));

        let error = abandon_import_attempt(
            &conn,
            &request,
            &lease,
            ImportAttemptAbandonment {
                marker_fence: None,
                identity_state: "failed",
                last_error_code: "LBR-AGENT-018",
                now_ms: 2,
            },
            commit_deadline_after(Duration::from_secs(1)),
        )
        .await
        .expect_err("expired workspace scope must not abandon import state");
        assert!(format!("{error:#}").contains("workspace lease"));

        let after_expiry_claims = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'scope-expiry-after-expiry'"
                    .to_string(),
            ))
            .await
            .expect("count blocked import claim")
            .expect("blocked import count row");
        assert_eq!(
            after_expiry_claims
                .try_get_by::<i64, _>("n")
                .expect("decode blocked import claim count"),
            0,
            "expired scope must not create a new import coverage claim"
        );
        let claim = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, owner FROM agent_coverage_claim
                 WHERE logical_turn_key = 'scope-expiry-reserved'"
                    .to_string(),
            ))
            .await
            .expect("read reserved import claim")
            .expect("reserved import claim");
        assert_eq!(
            claim.try_get_by::<String, _>("state").expect("claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim
                .try_get_by::<Option<String>, _>("owner")
                .expect("claim owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        let identity = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, owner, fence_token FROM agent_import_identity
                 WHERE identity_id = 'scope-expiry-identity'"
                    .to_string(),
            ))
            .await
            .expect("read reserved import identity")
            .expect("reserved import identity");
        assert_eq!(
            identity
                .try_get_by::<String, _>("state")
                .expect("identity state"),
            "leased"
        );
        assert_eq!(
            identity
                .try_get_by::<Option<String>, _>("owner")
                .expect("identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            identity
                .try_get_by::<Option<i64>, _>("fence_token")
                .expect("identity fence"),
            Some(lease.fence_token)
        );
    }

    /// Binding, lifecycle finalization, and error finalization each perform
    /// companion marker/catalog writes after their identity predicate. Expire
    /// the workspace from those final companion DMLs so the commit fence must
    /// roll the whole import attempt back, rather than relying on a timing
    /// window between an earlier predicate and COMMIT.
    #[tokio::test]
    async fn post_mutation_scope_expiry_rolls_back_import_marker_and_catalog_finalizers() {
        let dir = tempfile::tempdir().expect("create post-mutation import fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create post-mutation import database");
        let scope = CaptureScope {
            repo_id: "post-mutation-import-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("post-mutation-import-workspace".to_string()),
            workspace_fence: Some(17),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                       'active', 'post-mutation-import-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live post-mutation import workspace");
        let source_commitment = format!("source/hmac-v2/{}", "e".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "post-mutation-import-provider".to_string(),
            session_id: build_ai_session_id(
                capture_provider_name(AgentKind::ClaudeCode),
                "post-mutation-import-provider",
            ),
            source_kind: "file".to_string(),
            source_id: source_commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "post-mutation-import-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: source_commitment,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let deadline = || commit_deadline_after(std::time::Duration::from_secs(1));
        let lease = acquire_identity(
            &conn,
            &request,
            "post-mutation-import-writer",
            1,
            deadline(),
        )
        .await
        .expect("acquire scoped import identity while workspace is live");
        let turn = NormalizedTurn {
            logical_turn_key: "post-mutation-import-turn".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: Vec::new(),
        };
        let reserved = coverage_gate::reserve_import_turn_claims_until(
            &conn,
            &scope,
            &request.session_id,
            std::slice::from_ref(&turn),
            &lease.owner,
            2,
            deadline(),
        )
        .await
        .expect("reserve scoped import claim while workspace is live");
        let claim = reserved
            .reserved
            .first()
            .expect("one reserved scoped import claim")
            .clone();
        let checkpoint_id = "post-mutation-import-checkpoint";

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_import_marker_bind
             AFTER INSERT ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
               AND NEW.target = 'claude__post-mutation-import-provider'
               AND NEW.key = 'post-mutation-import-checkpoint'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'post-mutation-import-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-marker-bind expiry trigger");
        let error = bind_attempt(
            &conn,
            &request,
            &lease,
            &claim,
            checkpoint_id,
            3,
            deadline(),
        )
        .await
        .expect_err("expiry after prebound marker write must roll back attempt binding");
        assert!(format!("{error:#}").contains("workspace lease"));
        let identity_after_marker_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_import_identity
                 WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after post-marker rollback")
            .expect("identity remains after post-marker rollback");
        assert_eq!(
            identity_after_marker_expiry
                .try_get_by::<String, _>("state")
                .expect("decode post-marker identity state"),
            "leased"
        );
        assert_eq!(
            identity_after_marker_expiry
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode post-marker identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            identity_after_marker_expiry
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode post-marker identity attempt"),
            None
        );
        let claim_after_marker_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    request.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read claim after post-marker rollback")
            .expect("claim remains after post-marker rollback");
        assert_eq!(
            claim_after_marker_expiry
                .try_get_by::<String, _>("state")
                .expect("decode post-marker claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim_after_marker_expiry
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode post-marker claim attempt"),
            None
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                &request.session_id,
                checkpoint_id,
            )
            .await
            .expect("read post-marker rollback marker")
            .is_none(),
            "post-marker expiry must not publish a prebound marker"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-marker expiry trigger must roll back with the attempt");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER expire_scope_after_import_marker_bind".to_string(),
        ))
        .await
        .expect("remove post-marker-bind expiry trigger");

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_import_lifecycle
             AFTER UPDATE ON agent_session
             WHEN NEW.session_id = 'claude__post-mutation-import-provider'
               AND NEW.state = 'active'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'post-mutation-import-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-lifecycle expiry trigger");
        let error =
            finalize_noop_identity(&conn, &request, &lease, "committed", None, 4, deadline())
                .await
                .expect_err(
                    "expiry after lifecycle companion must roll back identity finalization",
                );
        assert!(format!("{error:#}").contains("workspace lease"));
        let identity_after_lifecycle_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner FROM agent_import_identity WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after post-lifecycle rollback")
            .expect("identity remains after post-lifecycle rollback");
        assert_eq!(
            identity_after_lifecycle_expiry
                .try_get_by::<String, _>("state")
                .expect("decode post-lifecycle identity state"),
            "leased"
        );
        assert_eq!(
            identity_after_lifecycle_expiry
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode post-lifecycle identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        let session_state_after_lifecycle_expiry: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read session after post-lifecycle rollback")
            .expect("session remains after post-lifecycle rollback")
            .try_get_by("state")
            .expect("decode post-lifecycle session state");
        assert_eq!(
            session_state_after_lifecycle_expiry, "pending",
            "post-lifecycle expiry must roll catalog state back"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-lifecycle expiry trigger must roll back with the import");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER expire_scope_after_import_lifecycle".to_string(),
        ))
        .await
        .expect("remove post-lifecycle expiry trigger");

        let marker = bind_attempt(
            &conn,
            &request,
            &lease,
            &claim,
            checkpoint_id,
            5,
            deadline(),
        )
        .await
        .expect("bind import attempt while workspace lease is live");
        let marker_generation = marker
            .generation
            .as_deref()
            .expect("prebound marker carries a generation")
            .to_string();
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_import_session_cleanup
             AFTER DELETE ON agent_session
             WHEN OLD.session_id = 'claude__post-mutation-import-provider'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'post-mutation-import-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-finalizer catalog expiry trigger");
        let error = abandon_import_attempt(
            &conn,
            &request,
            &lease,
            ImportAttemptAbandonment {
                marker_fence: Some((checkpoint_id, &marker_generation)),
                identity_state: "failed",
                last_error_code: "LBR-AGENT-018",
                now_ms: 6,
            },
            deadline(),
        )
        .await
        .expect_err("expiry after finalizer catalog cleanup must roll back the attempt");
        assert!(format!("{error:#}").contains("workspace lease"));
        let identity_after_finalizer_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_import_identity
                 WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after finalizer rollback")
            .expect("identity remains after finalizer rollback");
        assert_eq!(
            identity_after_finalizer_expiry
                .try_get_by::<String, _>("state")
                .expect("decode finalizer identity state"),
            "writing"
        );
        assert_eq!(
            identity_after_finalizer_expiry
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode finalizer identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            identity_after_finalizer_expiry
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode finalizer identity attempt")
                .as_deref(),
            Some(checkpoint_id)
        );
        let claim_after_finalizer_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    request.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read claim after finalizer rollback")
            .expect("claim remains after finalizer rollback");
        assert_eq!(
            claim_after_finalizer_expiry
                .try_get_by::<String, _>("state")
                .expect("decode finalizer claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim_after_finalizer_expiry
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode finalizer claim owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            claim_after_finalizer_expiry
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode finalizer claim attempt")
                .as_deref(),
            Some(checkpoint_id)
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                &request.session_id,
                checkpoint_id,
            )
            .await
            .expect("read marker after finalizer rollback")
            .is_some(),
            "post-finalizer expiry must restore the prebound marker"
        );
        let remaining_session_rows: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("count session after finalizer rollback")
            .expect("session count row")
            .try_get_by("n")
            .expect("decode session count");
        assert_eq!(
            remaining_session_rows, 1,
            "post-finalizer expiry must roll back provisional catalog deletion"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-finalizer expiry trigger must roll back with the import");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER expire_scope_after_import_session_cleanup".to_string(),
        ))
        .await
        .expect("remove post-finalizer catalog expiry trigger");

        // The primary monotonic half is still live, but its immutable SQLite
        // half already elapsed. Cleanup can stage all of its fenced DML, yet
        // the paired final authorization must roll it back rather than
        // re-anchor the expired SQLite deadline to fresh recovery grace.
        let error = abandon_import_attempt(
            &conn,
            &request,
            &lease,
            ImportAttemptAbandonment {
                marker_fence: Some((checkpoint_id, &marker_generation)),
                identity_state: "failed",
                last_error_code: "LBR-AGENT-018",
                now_ms: 7,
            },
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(5), 0),
        )
        .await
        .expect_err("expired primary SQLite deadline must roll back cleanup DML");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        let identity_after_sqlite_deadline = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_import_identity
                 WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after SQLite-deadline cleanup rollback")
            .expect("identity remains after SQLite-deadline cleanup rollback");
        assert_eq!(
            identity_after_sqlite_deadline
                .try_get_by::<String, _>("state")
                .expect("decode SQLite-deadline identity state"),
            "writing"
        );
        assert_eq!(
            identity_after_sqlite_deadline
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode SQLite-deadline identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            identity_after_sqlite_deadline
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode SQLite-deadline identity attempt")
                .as_deref(),
            Some(checkpoint_id)
        );
        let claim_after_sqlite_deadline = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, attempt_checkpoint_id FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    request.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read claim after SQLite-deadline cleanup rollback")
            .expect("claim remains after SQLite-deadline cleanup rollback");
        assert_eq!(
            claim_after_sqlite_deadline
                .try_get_by::<String, _>("state")
                .expect("decode SQLite-deadline claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim_after_sqlite_deadline
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode SQLite-deadline claim owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        assert_eq!(
            claim_after_sqlite_deadline
                .try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                .expect("decode SQLite-deadline claim attempt")
                .as_deref(),
            Some(checkpoint_id)
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                &request.session_id,
                checkpoint_id,
            )
            .await
            .expect("read marker after SQLite-deadline cleanup rollback")
            .is_some(),
            "expired primary SQLite deadline must retain the prebound marker"
        );
        let session_rows: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("count session after SQLite-deadline cleanup rollback")
            .expect("session count row")
            .try_get_by("n")
            .expect("decode SQLite-deadline session count");
        assert_eq!(
            session_rows, 1,
            "expired primary SQLite deadline must roll back provisional-session deletion"
        );
    }

    #[tokio::test]
    async fn stale_finalizer_cannot_delete_new_takeover_session_or_marker() {
        let dir = tempfile::tempdir().expect("create import finalizer fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import finalizer database");
        let session_id = "claude__takeover";
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at,
                stopped_at, schema_version, repo_id, worktree_id, scope_state
             ) VALUES (?, 'claude_code', 'takeover', 'active', ?, ?, '{}', 1, 1, NULL, 1,
                       'test-repo', '', 'scoped')",
            [
                session_id.into(),
                dir.path().to_string_lossy().into_owned().into(),
                serde_json::json!({"import_provisional": true})
                    .to_string()
                    .into(),
            ],
        ))
        .await
        .expect("seed provisional session");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind,
                source_id, schema_version, next_ordinal, state, owner,
                lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, scope_state
             ) VALUES ('identity', 'claude_code', 'takeover', 'file', 'source',
                       1, 0, 'leased', 'owner-b', 9999999999999, 2, 1, 1,
                       'test-repo', '', 'scoped')",
            [],
        ))
        .await
        .expect("seed takeover identity");
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "takeover".to_string(),
            session_id: session_id.to_string(),
            source_kind: "file".to_string(),
            source_id: "source".to_string(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            content_digest: "digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "repo".to_string(),
            capture_scope: Some(CaptureScope {
                repo_id: "test-repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            }),
            source_fingerprint: "source".to_string(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let stale_lease = ImportLease {
            identity_id: "identity".to_string(),
            owner: "owner-a".to_string(),
            fence_token: 1,
        };
        let checkpoint_id = "takeover-checkpoint";
        let stale_marker = TracesInflightMarker::new(session_id, checkpoint_id, 1);
        let stale_generation = stale_marker
            .generation
            .as_deref()
            .expect("stale marker generation")
            .to_string();
        let takeover_marker = TracesInflightMarker::new(session_id, checkpoint_id, 2);
        let takeover_generation = takeover_marker
            .generation
            .as_deref()
            .expect("takeover marker generation")
            .to_string();
        history::write_traces_inflight_marker(&conn, &takeover_marker)
            .await
            .expect("seed takeover marker");

        abandon_import_attempt(
            &conn,
            &request,
            &stale_lease,
            ImportAttemptAbandonment {
                marker_fence: Some((checkpoint_id, &stale_generation)),
                identity_state: "failed",
                last_error_code: "LBR-AGENT-018",
                now_ms: 2,
            },
            commit_deadline_after(Duration::from_secs(1)),
        )
        .await
        .expect("run stale finalizer");

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = ?",
                [session_id.into()],
            ))
            .await
            .expect("query preserved session")
            .expect("count row");
        assert_eq!(row.try_get_by::<i64, _>("n").expect("decode count"), 1);
        let marker = crate::internal::metadata::MetadataKv::get_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            checkpoint_id,
        )
        .await
        .expect("query takeover marker")
        .expect("takeover marker remains");
        let marker: TracesInflightMarker =
            serde_json::from_str(&marker.value).expect("decode takeover marker");
        assert_eq!(
            marker.generation.as_deref(),
            Some(takeover_generation.as_str())
        );

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER fail_import_abandon
             BEFORE UPDATE ON agent_import_identity
             WHEN OLD.identity_id = 'identity'
             BEGIN SELECT RAISE(FAIL, 'forced abandon failure'); END"
                .to_string(),
        ))
        .await
        .expect("install abandon failure trigger");
        let takeover_lease = ImportLease {
            identity_id: "identity".to_string(),
            owner: "owner-b".to_string(),
            fence_token: 2,
        };
        let surfaced = abandon_import_attempt_after_error(
            &conn,
            &request,
            &takeover_lease,
            None,
            "failed",
            anyhow!("primary import failure"),
            commit_deadline_after(Duration::from_secs(1)),
        )
        .await;
        let surfaced = format!("{surfaced:#}");
        assert!(surfaced.contains("primary import failure"), "{surfaced}");
        assert!(
            !surfaced.contains("forced abandon failure"),
            "cleanup diagnostics must not disclose a database error chain: {surfaced}"
        );
        assert!(surfaced.contains("agent doctor --repair"), "{surfaced}");
    }

    /// Cleanup may release only its own provisional claim/identity, and must
    /// not inherit SQLite's normal 30-second busy wait whether the primary
    /// import deadline is still live or already expired. If another writer
    /// holds the database, preserve recovery ownership and return the original
    /// failure with doctor advice.
    #[tokio::test]
    async fn import_cleanup_is_bounded_by_recovery_grace_under_writer_contention() {
        let dir = tempfile::tempdir().expect("create import cleanup deadline fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import cleanup deadline database");
        let lock_conn = crate::internal::db::establish_connection(&db_path.to_string_lossy())
            .await
            .expect("open independent import cleanup lock connection");
        let scope = CaptureScope {
            repo_id: "import-cleanup-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-cleanup-deadline-provider".to_string(),
            session_id: "claude__import_cleanup_deadline".to_string(),
            source_kind: "file".to_string(),
            source_id: "import-cleanup-deadline-source".to_string(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            content_digest: "import-cleanup-deadline-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "import-cleanup-deadline-repository".to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: "import-cleanup-deadline-fingerprint".to_string(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let lease = ImportLease {
            identity_id: "import-cleanup-deadline-identity".to_string(),
            owner: "import-cleanup-deadline-owner".to_string(),
            fence_token: 1,
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, scope_state
             ) VALUES (?, 'claude_code', ?, 'active', ?, '{}', '{}', 1, 1, 1, ?, ?, 'scoped')",
            [
                request.session_id.clone().into(),
                request.provider_session_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
            ],
        ))
        .await
        .expect("seed import cleanup session");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version, coverage_digest,
                completeness, revision, state, attempt_checkpoint_id, owner,
                lease_expires_at, fence_token, checkpoint_id, traces_commit,
                source_channel, created_at, updated_at
             ) VALUES (?, 'import-cleanup-deadline-turn', ?, 'digest', 'complete', 0,
                       'reserved_import', NULL, ?, 9999999999999, 1, NULL, NULL,
                       'import', 1, 1)",
            [
                request.session_id.clone().into(),
                crate::internal::ai::observed_agents::COVERAGE_SCHEMA_VERSION.into(),
                lease.owner.clone().into(),
            ],
        ))
        .await
        .expect("seed reserved import cleanup claim");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, observed_digest, next_ordinal, state, owner,
                lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, scope_state
             ) VALUES (?, 'claude_code', ?, ?, ?, 1, ?, 0, 'leased', ?,
                       9999999999999, ?, 1, 1, ?, ?, 'scoped')",
            [
                lease.identity_id.clone().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                request.content_digest.clone().into(),
                lease.owner.clone().into(),
                lease.fence_token.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
            ],
        ))
        .await
        .expect("seed leased import cleanup identity");

        let holder = crate::internal::db::begin_write_transaction(&lock_conn)
            .await
            .expect("hold SQLite writer during cleanup");
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        let surfaced = tokio::time::timeout(
            Duration::from_secs(2),
            abandon_import_attempt_after_error(
                &conn,
                &request,
                &lease,
                None,
                "failed",
                anyhow!("primary import failure"),
                expired,
            ),
        )
        .await;
        holder
            .rollback()
            .await
            .expect("release SQLite writer after cleanup timeout");
        let surfaced = surfaced.expect("expired cleanup must stop at its short recovery grace");
        let surfaced = format!("{surfaced:#}");
        assert!(surfaced.contains("primary import failure"), "{surfaced}");
        assert!(surfaced.contains("agent doctor --repair"), "{surfaced}");

        // A still-live primary deadline must not turn recovery cleanup into a
        // wait for the entire remaining import budget.
        let holder = crate::internal::db::begin_write_transaction(&lock_conn)
            .await
            .expect("hold SQLite writer during live-deadline cleanup");
        let surfaced = tokio::time::timeout(
            Duration::from_secs(2),
            abandon_import_attempt_after_error(
                &conn,
                &request,
                &lease,
                None,
                "failed",
                anyhow!("primary import failure while deadline is live"),
                commit_deadline_after(Duration::from_secs(5)),
            ),
        )
        .await;
        holder
            .rollback()
            .await
            .expect("release SQLite writer after live-deadline cleanup timeout");
        let surfaced =
            surfaced.expect("live-deadline cleanup must stop at its short recovery grace");
        let surfaced = format!("{surfaced:#}");
        assert!(
            surfaced.contains("primary import failure while deadline is live"),
            "{surfaced}"
        );
        assert!(surfaced.contains("agent doctor --repair"), "{surfaced}");

        let identity = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner FROM agent_import_identity WHERE identity_id = ?",
                [lease.identity_id.clone().into()],
            ))
            .await
            .expect("read identity after bounded cleanup")
            .expect("seeded identity remains");
        assert_eq!(
            identity
                .try_get_by::<String, _>("state")
                .expect("identity state"),
            "leased"
        );
        assert_eq!(
            identity
                .try_get_by::<Option<String>, _>("owner")
                .expect("identity owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
        let claim = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = 'import-cleanup-deadline-turn'",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read claim after bounded cleanup")
            .expect("seeded claim remains");
        assert_eq!(
            claim.try_get_by::<String, _>("state").expect("claim state"),
            "reserved_import"
        );
        assert_eq!(
            claim
                .try_get_by::<Option<String>, _>("owner")
                .expect("claim owner")
                .as_deref(),
            Some(lease.owner.as_str())
        );
    }

    #[test]
    fn opencode_real_nested_time_shape_is_collected() {
        let facts = collect_facts(
            AgentKind::OpenCode,
            br#"{
                "info": {
                    "id": "ses_time",
                    "directory": "/repo",
                    "time": {"created": 1784077200000, "updated": 1784077260000}
                },
                "messages": [{
                    "info": {
                        "role": "user",
                        "id": "msg_1",
                        "time": {"created": 1784077210000}
                    },
                    "parts": [{"type": "text", "text": "hello"}]
                }]
            }"#,
            None,
        )
        .expect("parse OpenCode facts");
        assert_eq!(
            facts.timestamps,
            vec![1_784_077_200, 1_784_077_260, 1_784_077_210]
        );
    }

    #[test]
    fn import_progress_error_display_and_debug_are_content_free() {
        let marker = "provider-session-/private/customer/secret";
        let error = ImportProgressError {
            summary: ImportSummary {
                session_id: marker.to_string(),
                agent_kind: "claude_code".to_string(),
                turns_seen: 1,
                checkpoints_written: 1,
                skipped_covered: 0,
                skipped_inflight: 0,
                conflicted: 0,
                partial: true,
            },
            subagent_checkpoints_written: 0,
            import_identity_id: marker.to_string(),
            import_fence_token: 7,
        };
        let rendered = format!("{error:#}");
        let debug = format!("{error:?}");
        assert!(rendered.contains("durable partial progress"));
        assert!(!rendered.contains(marker));
        assert!(!debug.contains(marker));
    }

    #[tokio::test]
    async fn descriptor_reader_refuses_an_embedded_host_without_spawning() {
        let fixture = tempfile::NamedTempFile::new().expect("create descriptor fixture");
        let file = std::fs::File::open(fixture.path()).expect("open descriptor fixture");
        let (result, raw_bytes) =
            crate::internal::ai::authorized_read::with_no_test_helper_program(
                read_authorized_descriptor_until(
                    file,
                    64,
                    Instant::now() + std::time::Duration::from_secs(1),
                ),
            )
            .await;

        assert_eq!(raw_bytes, 0);
        assert!(matches!(
            result
                .expect_err("an embedded host has no registered Libra helper")
                .downcast_ref::<ImportError>(),
            Some(ImportError::AuthorizedReaderUnavailable)
        ));
    }

    #[test]
    fn descriptor_reader_rejects_payload_bearing_status_two_without_leaking_it() {
        let marker = b"raw-helper-error-/private/customer/session";
        let mut frame = vec![2];
        frame.extend_from_slice(&0u64.to_le_bytes());
        frame.extend_from_slice(marker);
        let (result, raw_bytes) = decode_authorized_descriptor_frame(&frame, 128);

        assert_eq!(raw_bytes, 0);
        let rendered = format!(
            "{:#}",
            result.expect_err("status 2 must never carry a helper payload")
        );
        assert!(rendered.contains("authorized transcript reader failed"));
        assert!(!rendered.contains("customer"));
        assert!(!rendered.contains("session"));
    }

    #[cfg(unix)]
    fn write_descriptor_reader_fixture(
        directory: &tempfile::TempDir,
        name: &str,
        script: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.path().join(name);
        std::fs::write(&helper, script).expect("write descriptor reader fixture");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read descriptor reader fixture permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make descriptor reader fixture executable");
        helper
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_reader_accepts_only_bounded_complete_or_oversize_frames() {
        let directory = tempfile::tempdir().expect("create descriptor reader fixture");
        let complete = write_descriptor_reader_fixture(
            &directory,
            "complete-reader.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf '\\000\\004\\000\\000\\000\\000\\000\\000\\000safe'\n",
        );
        let source = directory.path().join("source");
        std::fs::write(&source, b"input").expect("write descriptor input");
        let (result, raw_bytes) = crate::internal::ai::authorized_read::with_test_helper_program(
            complete,
            read_authorized_descriptor_until(
                std::fs::File::open(&source).expect("open descriptor input"),
                16,
                Instant::now() + std::time::Duration::from_secs(2),
            ),
        )
        .await;
        assert_eq!(raw_bytes, 4);
        assert_eq!(result.expect("complete helper frame"), b"safe");

        let oversize = write_descriptor_reader_fixture(
            &directory,
            "oversize-reader.sh",
            "#!/bin/sh\nprintf '\\001\\005\\000\\000\\000\\000\\000\\000\\000'\n",
        );
        let (result, raw_bytes) = crate::internal::ai::authorized_read::with_test_helper_program(
            oversize,
            read_authorized_descriptor_until(
                std::fs::File::open(&source).expect("reopen descriptor input"),
                4,
                Instant::now() + std::time::Duration::from_secs(2),
            ),
        )
        .await;
        assert_eq!(raw_bytes, 5);
        assert!(result
            .expect_err("cap + 1 frame must be rejected")
            .downcast_ref::<super::super::observed_agents::transcript_source::TranscriptReadError>()
            .is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_reader_outer_cancellation_kills_and_reaps_the_helper() {
        let directory = tempfile::tempdir().expect("create stalled descriptor reader fixture");
        let pid_file = directory.path().join("descriptor-reader.pid");
        let escaped_pid_file = pid_file.to_string_lossy().replace('\'', "'\"'\"'");
        let helper = write_descriptor_reader_fixture(
            &directory,
            "stalled-reader.sh",
            &format!("#!/bin/sh\nprintf '%s' \"$$\" > '{escaped_pid_file}'\nexec /bin/sleep 60\n"),
        );
        let source = directory.path().join("source");
        std::fs::write(&source, b"input").expect("write descriptor input");
        let reader = tokio::spawn(
            crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                read_authorized_descriptor_until(
                    std::fs::File::open(&source).expect("open descriptor input"),
                    64,
                    Instant::now() + std::time::Duration::from_secs(20),
                ),
            ),
        );
        let pid = async {
            for _ in 0..100 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("live helper must publish its pid before outer cancellation");
        // Aborting the caller task models an outer runtime deadline dropping
        // the reader future after the helper is known to be live.
        reader.abort();
        assert!(
            reader
                .await
                .expect_err("aborting the reader task must report cancellation")
                .is_cancelled(),
            "outer cancellation must interrupt the live descriptor helper"
        );
        let reaped = async {
            for _ in 0..100 {
                // SAFETY: signal zero probes the exact test child PID only.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "outer-cancelled descriptor helper was not reaped");
    }

    /// The private descriptor reader receives an already-authorized raw file
    /// as stdin. Its leader may exit after forking a child that retains both
    /// that descriptor and stdout; dropping the reader must terminate the
    /// process group rather than only the now-zombie leader.
    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_reader_outer_cancellation_kills_descendant_holding_raw_descriptor() {
        let directory = tempfile::tempdir().expect("create descriptor fixture");
        let descendant_file = directory.path().join("descriptor-descendant.pid");
        let escaped_descendant = descendant_file.to_string_lossy().replace('\'', "'\"'\"'");
        let helper = write_descriptor_reader_fixture(
            &directory,
            "forking-reader.sh",
            &format!(
                "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s' \"$!\" > '{escaped_descendant}'\nexit 0\n"
            ),
        );
        let source = directory.path().join("source");
        std::fs::write(&source, b"input").expect("write descriptor input");
        let reader = tokio::spawn(
            crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                read_authorized_descriptor_until(
                    std::fs::File::open(&source).expect("open descriptor input"),
                    64,
                    Instant::now() + std::time::Duration::from_secs(20),
                ),
            ),
        );
        let descendant = async {
            for _ in 0..100 {
                if let Ok(value) = std::fs::read_to_string(&descendant_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("forked reader descendant must publish its PID");
        reader.abort();
        assert!(
            reader
                .await
                .expect_err("reader task must report cancellation")
                .is_cancelled(),
            "outer cancellation must interrupt the descriptor reader"
        );
        let reaped = async {
            for _ in 0..100 {
                // SAFETY: signal zero only probes the exact fixture PID.
                if unsafe { libc::kill(descendant, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "outer cancellation left a raw-descriptor helper descendant alive"
        );
    }

    #[test]
    fn future_source_or_normalized_timestamps_are_rejected_before_chronology() {
        let now = 1_700_000_000;
        let facts = SourceFacts {
            timestamps: vec![now + IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS + 1],
            ..Default::default()
        };
        let turns = vec![NormalizedTurn {
            logical_turn_key: "future".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: Vec::new(),
        }];
        assert!(matches!(
            build_import_chronology(&facts, &turns, now)
                .expect_err("a source timestamp beyond skew must be rejected")
                .downcast_ref::<ImportError>(),
            Some(ImportError::FutureTimestamp)
        ));

        let facts = SourceFacts::default();
        let turns = vec![NormalizedTurn {
            logical_turn_key: "future-turn".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: Some(now + IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS + 1),
            ended_at: None,
            records: Vec::new(),
        }];
        assert!(matches!(
            build_import_chronology(&facts, &turns, now)
                .expect_err("a normalized timestamp beyond skew must be rejected")
                .downcast_ref::<ImportError>(),
            Some(ImportError::FutureTimestamp)
        ));
    }

    #[test]
    fn synthesized_future_turn_boundary_is_rejected() {
        let now = 1_700_000_000;
        let turns = (0..=(IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS as usize + 1))
            .map(|ordinal| NormalizedTurn {
                logical_turn_key: format!("synthetic-{ordinal}"),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: Vec::new(),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            build_import_chronology(&SourceFacts::default(), &turns, now)
                .expect_err("ordinal fallback must not synthesize a future persisted timestamp")
                .downcast_ref::<ImportError>(),
            Some(ImportError::FutureTimestamp)
        ));
    }

    #[tokio::test]
    async fn future_request_is_rejected_before_any_import_identity_or_session_mutation() {
        let directory = tempfile::tempdir().expect("create future request fixture");
        let database = directory.path().join("libra.db");
        let conn = crate::internal::db::create_database(&database.to_string_lossy())
            .await
            .expect("create future request database");
        let future = Utc::now().timestamp() + IMPORT_FUTURE_TIMESTAMP_SKEW_SECONDS + 60;
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "future-provider".to_string(),
            session_id: "future-session".to_string(),
            source_kind: "file".to_string(),
            source_id: "source".to_string(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            content_digest: "digest".to_string(),
            started_at: future,
            ended_at: future,
            session_state: "stopped".to_string(),
            stopped_at: Some(future),
            working_dir: directory.path().to_path_buf(),
            repository_identity: "repository".to_string(),
            capture_scope: None,
            source_fingerprint: "legacy-ownership-fingerprint".to_string(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({}),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::from([(
                "future-turn".to_string(),
                TurnBoundary {
                    started_at: future,
                    ended_at: future,
                },
            )]),
            turns: Vec::new(),
        };
        let error = import_prepared_with_subagent_discovery(
            &conn,
            directory.path(),
            request,
            commit_deadline_after(std::time::Duration::from_secs(5)),
            super::super::subagent_content::SubagentDiscovery::default(),
        )
        .await
        .expect_err("future request must be rejected before leasing");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::FutureTimestamp)
        ));
        for table in ["agent_import_identity", "agent_session"] {
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    format!("SELECT COUNT(*) AS n FROM {table}"),
                ))
                .await
                .expect("count unmodified import table")
                .expect("count row");
            assert_eq!(
                row.try_get_by::<i64, _>("n")
                    .expect("decode unmodified table count"),
                0,
                "future rejection must not mutate {table}"
            );
        }
    }

    #[tokio::test]
    async fn legacy_v1_request_cannot_reach_any_durable_import_sink() {
        let directory = tempfile::tempdir().expect("create legacy persistence fixture");
        let database = directory.path().join("libra.db");
        let conn = crate::internal::db::create_database(&database.to_string_lossy())
            .await
            .expect("create legacy persistence database");
        let now = Utc::now().timestamp();
        let raw_locator = "project/private-session.jsonl";
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "legacy-persistence-provider".to_string(),
            session_id: "legacy-persistence-session".to_string(),
            source_kind: "file".to_string(),
            source_id: raw_locator.to_string(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V1,
            content_digest: "legacy-digest".to_string(),
            started_at: now,
            ended_at: now,
            session_state: "stopped".to_string(),
            stopped_at: Some(now),
            working_dir: directory.path().to_path_buf(),
            repository_identity: "legacy-repository-proof".to_string(),
            capture_scope: None,
            source_fingerprint: legacy_import_source_fingerprint(
                AgentKind::ClaudeCode,
                "file",
                raw_locator,
            ),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({}),
            transcript_snapshot: None,
            turn_boundaries: BTreeMap::new(),
            turns: Vec::new(),
        };
        let error = import_prepared_with_subagent_discovery(
            &conn,
            directory.path(),
            request,
            commit_deadline_after(std::time::Duration::from_secs(5)),
            super::super::subagent_content::SubagentDiscovery::default(),
        )
        .await
        .expect_err("a newly supplied V1 request must be rejected before persistence");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::SourceAuthorization)
        ));
        for table in ["agent_import_identity", "agent_session"] {
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    format!("SELECT COUNT(*) AS n FROM {table}"),
                ))
                .await
                .expect("count untouched persistence sink")
                .expect("count row");
            assert_eq!(
                row.try_get_by::<i64, _>("n")
                    .expect("decode untouched persistence count"),
                0,
                "legacy V1 request must not mutate {table}"
            );
        }
        let import_metadata = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope IN ('agent_traces_inflight', 'agent_import_index_repair')"
                    .to_string(),
            ))
            .await
            .expect("count untouched import metadata")
            .expect("import metadata count row");
        assert_eq!(
            import_metadata
                .try_get_by::<i64, _>("n")
                .expect("decode untouched import metadata count"),
            0,
            "legacy V1 request must not create import recovery metadata"
        );
    }
}
