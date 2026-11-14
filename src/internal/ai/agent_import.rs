//! Historical external-agent transcript import orchestration (plan-20260713 M4 / DR-05).
//!
//! The command layer discovers and authorizes a source, then hands the held
//! [`TranscriptSource`] to this module. Raw provider bytes stay in memory only:
//! they are strictly parsed into typed turns, redacted field-by-field, and
//! re-serialized as an allowlist-only per-turn projection before persistence.

#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(any(unix, windows))]
use super::authorized_read::{
    AUTHORIZED_READ_HELPER_ARG, AUTHORIZED_READ_HELPER_CAP_ENV, CancellationSafeChild,
    configure_private_helper_process_group, read_async_strictly_bounded, registered_helper_command,
};
use super::{
    capture::{
        catalog::{
            CaptureCatalogError, CaptureCatalogRedactionReport, CaptureCatalogSession,
            CaptureCatalogStore, CaptureImportSessionCommit, CaptureImportSessionLifecycleState,
            CaptureImportSessionPrepareRequest, CaptureImportSource,
        },
        checkpoint::{
            CheckpointRedactedPayload, CheckpointStore, CheckpointWriteOutcome,
            ImportedCheckpointWriteRequest, ImportedCheckpointWriter, ReasoningCoveragePlan,
            ReasoningIdentityPreparation, TracesCheckpointStore, partition_reasoning_for_import,
            prepare_reasoning_coverage_plan_from_disposition,
        },
        snapshot::{
            CaptureCoverageRuntime, CaptureSnapshot, CaptureSnapshotPartialReason,
            CaptureSnapshotPolicy, CaptureSnapshotProjection, CaptureSnapshotService,
        },
    },
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    observed_agents::{
        AgentKind, CanonValue, Completeness, CoverageSchemaVersion, ExportAuthorized,
        MAX_REDACTION_MATCH_SAMPLES, NormalizedTurn, RedactedBytes, RedactionReport, Redactor,
        TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource,
        builtin::hermes::{
            HermesNormalizeStatus, HermesReadLimits, HermesScope, normalize_hermes_jsonl_until,
        },
        codex_reasoning::{CodexRetention, CodexRetentionDto},
        coverage::{
            RedactedImportCoverageRuntimeDto, encode_redacted_import_runtime,
            normalize_codex_retained_rollout_until,
        },
        encode_coverage_turn_for_schema,
        live_capture::{
            LiveCaptureBinding, LiveCaptureContext, StoredSubagentSelection,
            SubagentDiscoveryLimits,
        },
        normalize_claude_transcript_until, normalize_codex_rollout_until,
        normalize_opencode_export_until, parse_canon_value,
        reasoning::{
            NativeProvenReasoning, ReasoningSourceDisposition,
            note_claude_attached_duplicates_until,
        },
        redact_turns_with_report, safe_turn_projection, safe_turn_projection_for_schema,
        select_coverage_schema,
    },
};
use crate::{
    internal::{
        ai::{
            coverage_gate::{
                self, ImportIdentityCommit, LiveCaptureClaimCommitPlan, LiveClaimCommitPlan,
                ReasoningCoverageInput, ReservedTurnClaim, VersionedLiveClaimCommitPlan,
            },
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
    utils::{json::normalize_json_value, util},
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
#[derive(Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) coverage_runtime: Option<RedactedImportCoverageRuntimeDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) codex_retention: Option<CodexRetentionDto>,
}

impl std::fmt::Debug for PreparedImportProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedImportProjection")
            .field("turn_count", &self.turns.len())
            .field("coverage_runtime_present", &self.coverage_runtime.is_some())
            .finish_non_exhaustive()
    }
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

    #[cfg(windows)]
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    #[cfg(windows)]
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        Vec::<u16>::deserialize(deserializer)
            .map(|units| PathBuf::from(OsString::from_wide(&units)))
    }

    #[cfg(not(any(unix, windows)))]
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        path.to_string_lossy().serialize(serializer)
    }

    #[cfg(not(any(unix, windows)))]
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
        selected_provider_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot;
}

impl ImportSnapshotPort for CaptureSnapshotService {
    fn capture_authorized(
        &self,
        source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        selected_provider_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        let Some(kind) = AgentKind::from_db_str(agent_kind) else {
            return CaptureSnapshotService::capture_authorized(
                source,
                agent_kind,
                libra_session_id,
                policy,
            );
        };
        let canonical_session =
            build_ai_session_id(capture_provider_name(kind), selected_provider_session_id);
        CaptureSnapshotService::capture_authorized_with_reasoning_context(
            source,
            crate::internal::ai::observed_agents::live_capture::selected_snapshot_adapter(kind),
            crate::internal::ai::capture::snapshot::AuthorizedReasoningSourceContext::verified_import(
                agent_kind, libra_session_id, &canonical_session, selected_provider_session_id,
            ),
            policy,
        )
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
        selected_provider_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        self.snapshot.capture_authorized(
            source,
            agent_kind,
            libra_session_id,
            selected_provider_session_id,
            policy,
        )
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

fn json_unix_seconds(value: Option<&serde_json::Value>) -> Option<i64> {
    let number = value?.as_f64()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let whole = number.trunc();
    if whole > 10_000_000_000.0 {
        Some((whole / 1000.0) as i64)
    } else {
        Some(whole as i64)
    }
}

/// Official Hermes JSONL facts. A parent id is not a second session, and a
/// non-default profile or non-cli source is refused before any turn is built.
/// The thinking blocks of a Pi frame as stored retained data: per block, the
/// key of the turn that holds it and its `thinking`, `thinkingSignature`, and
/// `redacted` fields exactly as the redacted session wrote them (absent
/// fields stay absent). Nothing is interpreted or joined to the answer.
fn pi_retained_thinking_json(
    thinking: &[super::observed_agents::builtin::pi::PiRetainedThinking],
) -> Result<serde_json::Value> {
    let raw = |value: &Option<super::observed_agents::builtin::pi::PiRawJson>| {
        value
            .as_ref()
            .map(|value| serde_json::from_str::<serde_json::Value>(value.get()))
            .transpose()
            .map_err(|_| ImportError::SourceAuthorization)
    };
    let mut blocks = Vec::with_capacity(thinking.len());
    for block in thinking {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "turn_key".to_string(),
            serde_json::json!(block.turn_key.as_deref()),
        );
        for (name, value) in [
            ("thinking", raw(&block.thinking)?),
            ("thinkingSignature", raw(&block.thinking_signature)?),
            ("redacted", raw(&block.redacted)?),
        ] {
            if let Some(value) = value {
                entry.insert(name.to_string(), value);
            }
        }
        blocks.push(serde_json::Value::Object(entry));
    }
    Ok(serde_json::Value::Array(blocks))
}

/// A Pi session's facts. The header, the file's first line, is the record:
/// it gives the session id and the working directory (`cwd`). Every entry's
/// `timestamp` gives the chronology. Pi writes no end-of-session entry, so
/// the session stays active and its last turn incomplete.
fn collect_pi_facts(
    bytes: &[u8],
    facts: &mut SourceFacts,
    deadline: Option<Instant>,
) -> Result<()> {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let header = match lines.next().map(parse_canon_value) {
        Some(Ok(header @ CanonValue::Object(_))) => header,
        _ => return Err(ImportError::SourceAuthorization.into()),
    };
    if header.get("type").and_then(CanonValue::as_str) != Some("session") {
        return Err(ImportError::SourceAuthorization.into());
    }
    let id = header
        .get("id")
        .and_then(CanonValue::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(ImportError::SessionIdentityConflict)?;
    facts.provider_session_ids.insert(id.to_string());
    let cwd = header
        .get("cwd")
        .and_then(CanonValue::as_str)
        .ok_or(ImportError::WorkingDirMissingOrAmbiguous)?;
    facts.working_dirs.insert(PathBuf::from(cwd));
    if let Some(timestamp) = header.get("timestamp").and_then(timestamp_seconds) {
        facts.timestamps.push(timestamp);
    }
    for line in lines {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(ImportError::DeadlineExceeded.into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // The normalizer accounts for a line it cannot read; the chronology
        // uses only lines that parse.
        let Ok(entry) = parse_canon_value(line) else {
            continue;
        };
        if let Some(timestamp) = entry.get("timestamp").and_then(timestamp_seconds) {
            facts.timestamps.push(timestamp);
        }
    }
    Ok(())
}

fn collect_hermes_facts(bytes: &[u8], facts: &mut SourceFacts) -> Result<()> {
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return Err(ImportError::SourceAuthorization.into());
    };
    if lines.next().is_some() {
        return Err(ImportError::SourceAuthorization.into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(first).map_err(|_| ImportError::SourceAuthorization)?;
    let session = value.as_object().ok_or(ImportError::SourceAuthorization)?;
    let source = session.get("source").and_then(serde_json::Value::as_str);
    let created = session
        .get("created_source")
        .and_then(serde_json::Value::as_str);
    if !source.is_some_and(|value| value.eq_ignore_ascii_case("cli"))
        || created.is_some_and(|value| !value.eq_ignore_ascii_case("cli"))
    {
        return Err(ImportError::SourceAuthorization.into());
    }
    if session
        .get("user_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|user| !user.is_empty())
    {
        return Err(ImportError::SourceAuthorization.into());
    }
    let profile = session
        .get("profile_name")
        .and_then(serde_json::Value::as_str);
    if profile.is_some_and(|name| !name.is_empty() && name != "default") {
        return Err(ImportError::SourceAuthorization.into());
    }
    let id = session
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(ImportError::SessionIdentityConflict)?;
    facts.provider_session_ids.insert(id.to_string());
    let cwd = session
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .ok_or(ImportError::WorkingDirMissingOrAmbiguous)?;
    facts.working_dirs.insert(PathBuf::from(cwd));
    if let Some(started) = json_unix_seconds(session.get("started_at")) {
        facts.timestamps.push(started);
    }
    if let Some(ended) = json_unix_seconds(session.get("ended_at")) {
        facts.timestamps.push(ended);
        facts.terminal = true;
    }
    if let Some(messages) = session
        .get("messages")
        .and_then(serde_json::Value::as_array)
    {
        for message in messages {
            if let Some(timestamp) = json_unix_seconds(message.get("timestamp")) {
                facts.timestamps.push(timestamp);
            }
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
        AgentKind::Hermes => collect_hermes_facts(bytes, &mut facts)?,
        AgentKind::Pi => collect_pi_facts(bytes, &mut facts, deadline)?,
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
                // Native exports bind the session directory under location;
                // classic exports use directory/cwd. Collect every declaration
                // so canonical scope validation can reject conflicts instead
                // of allowing one spelling to hide another.
                for cwd in [
                    info.get("directory"),
                    info.get("cwd"),
                    info.get("location")
                        .and_then(|location| location.get("directory")),
                ]
                .into_iter()
                .flatten()
                .filter_map(CanonValue::as_str)
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

fn source_digest_for_coverage_versions(
    turns: &[NormalizedTurn],
    versions: &[i64],
) -> Result<String> {
    if versions.len() != turns.len() {
        bail!("import coverage version count differs from turn count");
    }
    for (turn, version) in turns.iter().zip(versions) {
        encode_coverage_turn_for_schema(*version, turn)?;
    }
    if versions.iter().all(|version| *version == 1) {
        return Ok(source_digest(turns));
    }
    let mut digest = Sha256::new();
    digest.update(b"libra-agent-import-source-coverage-v2\0");
    for (turn, version) in turns.iter().zip(versions) {
        digest.update(version.to_be_bytes());
        update_length_delimited_import_preimage(&mut digest, turn.logical_turn_key.as_bytes())?;
        let encoded = encode_coverage_turn_for_schema(*version, turn)?;
        update_length_delimited_import_preimage(&mut digest, encoded.digest_hex.as_bytes())?;
    }
    Ok(hex::encode(digest.finalize()))
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
#[cfg_attr(windows, allow(dead_code))]
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
            | CaptureSnapshotPartialReason::SourceEmpty
            | CaptureSnapshotPartialReason::UnprovenOrdinary,
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
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, read_cap, deadline);
        // The raw descriptor reader relies on Unix process-group containment
        // for forked helper descendants. Do not spawn it where that contract
        // is unavailable.
        return (Err(ImportError::AuthorizedReaderUnavailable.into()), 0);
    }

    #[cfg(any(unix, windows))]
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
        #[cfg(unix)]
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return (Err(ImportError::AuthorizedReaderFailed.into()), 0),
        };
        #[cfg(unix)]
        let mut child = CancellationSafeChild::new_process_group(child);
        #[cfg(windows)]
        let mut child = match CancellationSafeChild::spawn_windows(&mut command) {
            Ok(child) => child,
            Err(_) => return (Err(ImportError::AuthorizedReaderFailed.into()), 0),
        };
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

/// Re-read ordinary Claude attachments on the authorized transcript. A
/// duplicate-key line adds no field. The transcript bytes stay in place.
fn retain_claude_attached_fields(
    agent_kind: AgentKind,
    transcript: &[u8],
    turns: &mut [NormalizedTurn],
    deadline: Instant,
) -> Result<()> {
    if agent_kind != AgentKind::ClaudeCode {
        return Ok(());
    }
    ensure_before_deadline(deadline)?;
    note_claude_attached_duplicates_until(turns, transcript, deadline)
        .ok_or(ImportError::DeadlineExceeded)?;
    Ok(())
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
        selected_provider_session_id,
        CaptureSnapshotPolicy::with_deadline(Some(deadline)),
    );
    let transcript_snapshot = snapshot.safe_projection();
    let snapshot_redaction_report = snapshot.redaction_report().clone();
    let transcript = snapshot
        .try_into_ordinary()
        .map_err(|_owned_rejection| ImportError::SourceAuthorization)?
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
    retain_claude_attached_fields(agent_kind, transcript.bytes(), &mut turns, deadline)?;
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
#[cfg(test)]
pub(crate) fn prepare_import_projection(
    agent_kind: AgentKind,
    expected_provider_commitment: [u8; 32],
    bytes: Vec<u8>,
    deadline: Instant,
) -> Result<PreparedImportProjection> {
    let (projection, native) = prepare_import_projection_with_reasoning(
        agent_kind,
        expected_provider_commitment,
        bytes,
        deadline,
    )?;
    if native.is_some() {
        return Err(ImportError::SourceAuthorization.into());
    }
    Ok(projection)
}

/// The descriptor helper's typed preparation. The native batch remains an
/// owned, non-serializable value until the private transport sealer consumes
/// it; only the redacted projection follows the existing ordinary wire path.
pub(crate) fn prepare_import_projection_with_reasoning(
    agent_kind: AgentKind,
    expected_provider_commitment: [u8; 32],
    bytes: Vec<u8>,
    deadline: Instant,
) -> Result<(PreparedImportProjection, Option<NativeProvenReasoning>)> {
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
    let storage_root = util::try_get_storage_path(Some(working_dir.clone()))
        .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?
        .canonicalize()
        .map_err(|_| ImportError::WorkingDirMissingOrAmbiguous)?;
    let storage_commitment = import_storage_commitment(&storage_root)?;
    let provisional_session_id =
        build_ai_session_id(capture_provider_name(agent_kind), provider_session_id);
    let capture_services = ImportCaptureServices::production();
    let mut snapshot = capture_services.capture_authorized(
        TranscriptSource::Bytes {
            auth: ExportAuthorized::issue(agent_kind.as_db_str(), &provisional_session_id, &bytes),
            bytes,
        },
        agent_kind.as_db_str(),
        &provisional_session_id,
        provider_session_id,
        CaptureSnapshotPolicy::with_deadline(Some(deadline)),
    );
    let mut runtime = snapshot.take_coverage_runtime();
    let snapshot_redaction_report = snapshot.redaction_report().clone();
    let (transcript_snapshot, transcript, reasoning) = snapshot.into_checkpoint_parts();
    let native = match reasoning {
        ReasoningSourceDisposition::Ordinary => None,
        ReasoningSourceDisposition::NativeProven(batch) => Some(batch),
        _ => return Err(ImportError::SourceAuthorization.into()),
    };
    let transcript = transcript.ok_or_else(|| snapshot_failure_for_import(&transcript_snapshot))?;
    ensure_before_deadline(deadline)?;
    let mut hermes_handoff = None;
    let mut pi_retained = None;
    let (mut turns, codex_slices, codex_unattributed_source) = if agent_kind == AgentKind::Codex {
        let normalized = normalize_codex_retained_rollout_until(transcript.bytes(), deadline)
            .ok_or(ImportError::DeadlineExceeded)?;
        (
            normalized.turns,
            Some(normalized.retained),
            normalized.unattributed_source,
        )
    } else if agent_kind == AgentKind::Pi {
        // The snapshot already bounded these bytes with the Pi worker frame
        // (PI-33); a refused frame left no transcript above. The same frame
        // here, under the import deadline, gives the ordinary turns and,
        // beside them, every thinking block as written in the redacted
        // session. An explicit ordinary import authorizes no reasoning
        // records, so the turns drop theirs and the thinking is kept as
        // retained, unproven data, as Hermes keeps its own.
        let frame = super::observed_agents::builtin::pi::pi_worker_frame_with_deadline(
            transcript.bytes(),
            &Some(deadline),
        )
        .map_err(|error| match error {
            super::observed_agents::builtin::pi::PiWorkerError::Cancelled => {
                ImportError::DeadlineExceeded
            }
            _ => ImportError::SourceAuthorization,
        })?;
        let turns = frame
            .ordinary
            .into_iter()
            .map(|mut turn| {
                turn.records.retain(|record| {
                    !matches!(
                        record,
                        super::observed_agents::SemanticRecord::Reasoning { .. }
                    )
                });
                turn
            })
            .collect();
        pi_retained = Some(pi_retained_thinking_json(&frame.thinking)?);
        (turns, None, false)
    } else if agent_kind == AgentKind::Hermes {
        let scope = HermesScope {
            session_id: provider_session_id.clone(),
            profile: "default".to_string(),
            repo_root: working_dir.clone(),
        };
        let normalized = normalize_hermes_jsonl_until(
            transcript.bytes(),
            &scope,
            &HermesReadLimits::default(),
            Some(deadline),
        )
        .ok_or(ImportError::DeadlineExceeded)?;
        let HermesNormalizeStatus::Ready(normalized) = normalized else {
            return Err(ImportError::SourceAuthorization.into());
        };
        if normalized.parent_associated || normalized.source_state != "unproven" {
            return Err(ImportError::SourceAuthorization.into());
        }
        let turns = normalized.turns;
        hermes_handoff = Some((normalized.retained, normalized.usage, normalized.partial));
        (turns, None, false)
    } else {
        let turns = match agent_kind {
            AgentKind::ClaudeCode => {
                normalize_claude_transcript_until(transcript.bytes(), deadline)
            }
            AgentKind::OpenCode => normalize_opencode_export_until(transcript.bytes(), deadline),
            _ => bail!(
                "agent kind '{}' is not importable",
                agent_kind.as_cli_slug()
            ),
        }
        .ok_or(ImportError::DeadlineExceeded)?;
        (turns, None, false)
    };
    retain_claude_attached_fields(agent_kind, transcript.bytes(), &mut turns, deadline)?;
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
    if hermes_handoff
        .as_ref()
        .is_some_and(|(_, _, source_partial)| *source_partial)
        && let Some(last) = turns.last_mut()
    {
        last.completeness = Completeness::Incomplete;
    }
    // Import lifecycle evidence owns the final active-turn fence for both
    // projections; typed snapshot completeness cannot override that fence.
    if !facts.terminal
        && let Some(last) = runtime.turns.as_mut().and_then(|turns| turns.last_mut())
    {
        last.completeness = Completeness::Incomplete;
    }
    if runtime.source_partial
        || (native.is_some()
            && runtime.turns.as_ref().is_some_and(|turns| {
                turns
                    .iter()
                    .any(|turn| select_coverage_schema(turn) == CoverageSchemaVersion::V2)
            }))
    {
        return Err(ImportError::SourceAuthorization.into());
    }
    let coverage_runtime = if agent_kind == AgentKind::OpenCode {
        Some(
            encode_redacted_import_runtime(
                agent_kind.as_db_str(),
                runtime.turns.as_deref(),
                runtime.classifier_rejected,
                runtime.source_partial,
                deadline,
            )
            .map_err(|_| ImportError::SourceAuthorization)?,
        )
    } else {
        None
    };
    let (turn_boundaries, started_at, ended_at) =
        build_import_chronology(&facts, &turns, trusted_now)?;
    let mut retention_unavailable = false;
    let codex_retention = codex_slices.map(|slices| {
        CodexRetentionDto::encode(&turns, slices)
            .unwrap_or_else(|_| {
                retention_unavailable = true;
                CodexRetentionDto::unavailable()
            })
            .with_summary(
                super::observed_agents::session_summary::from_redacted_codex_snapshot(
                    &transcript,
                    deadline,
                ),
            )
    });
    ensure_before_deadline(deadline)?;
    let mut redaction_report =
        merged_import_redaction_report(&snapshot_redaction_report, &typed_redaction_report);
    if agent_kind == AgentKind::Codex {
        match super::observed_agents::codex_reasoning::sidecar(transcript.bytes(), Some(deadline)) {
            Ok(Some(stats)) => super::observed_agents::codex_reasoning::merge_sidecar_report(
                &mut redaction_report,
                stats,
            ),
            Ok(None) => {}
            Err(_) => {
                ensure_before_deadline(deadline)?;
                redaction_report["codex_reasoning"] =
                    super::observed_agents::codex_reasoning::unavailable_statistics();
            }
        }
    }
    if retention_unavailable {
        super::observed_agents::codex_reasoning::mark_retention_unavailable(&mut redaction_report);
    }
    if let Some(retained) = pi_retained {
        redaction_report["pi_source_state"] = serde_json::json!("unproven");
        redaction_report["pi_retained"] = retained;
    }
    if let Some((retained, usage, _)) = hermes_handoff.as_ref() {
        redaction_report["hermes_source_state"] = serde_json::json!("unproven");
        redaction_report["hermes_parent_associated"] = serde_json::json!(false);
        redaction_report["hermes_retained"] =
            serde_json::to_value(retained).unwrap_or_else(|_| serde_json::json!([]));
        redaction_report["hermes_usage"] = match usage {
            Some(usage) => serde_json::to_value(usage).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
    }
    if agent_kind == AgentKind::Codex
        && (codex_unattributed_source
            || turns
                .iter()
                .map(|turn| turn.logical_turn_key.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                != turns.len())
    {
        if !redaction_report
            .get("codex_reasoning")
            .is_some_and(serde_json::Value::is_object)
        {
            redaction_report["codex_reasoning"] =
                super::observed_agents::codex_reasoning::unavailable_statistics();
        }
        redaction_report["codex_reasoning"]["turn_attribution_unproven"] = serde_json::json!(true);
        redaction_report["codex_reasoning"]["partial"] = serde_json::json!(true);
    }
    let projection = PreparedImportProjection {
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
        redaction_report: normalize_json_value(redaction_report),
        transcript_snapshot: Some(transcript_snapshot),
        turn_boundaries,
        turns,
        coverage_runtime,
        codex_retention,
    };
    Ok((projection, native))
}

/// Reconstruct the parent-owned compatibility fields after it has checked the
/// helper's fixed storage commitment and derived the opaque V2 source
/// commitment inside the scoped repository.
#[cfg_attr(windows, allow(dead_code))]
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
    bind_attempt_for_coverage(
        conn,
        request,
        lease,
        None,
        claim,
        checkpoint_id,
        now_ms,
        deadline,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
enum ImportAttemptBinding<'a> {
    Reserved {
        version: Option<CoverageSchemaVersion>,
        claim: &'a ReservedTurnClaim,
    },
    Retention(&'a coverage_gate::CommittedTurnClaimSnapshot),
}

#[allow(clippy::too_many_arguments)]
async fn bind_attempt_for_coverage(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    version: Option<CoverageSchemaVersion>,
    claim: &ReservedTurnClaim,
    checkpoint_id: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<TracesInflightMarker> {
    bind_import_attempt(
        conn,
        request,
        lease,
        ImportAttemptBinding::Reserved { version, claim },
        checkpoint_id,
        now_ms,
        deadline,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn bind_import_attempt(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    binding: ImportAttemptBinding<'_>,
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
    let coverage_bound = match binding {
        ImportAttemptBinding::Reserved { version, claim } => match version {
            Some(version) => {
                coverage_gate::bind_import_turn_claim_attempt_for_schema_with_conn(
                    &txn,
                    &request.session_id,
                    version,
                    claim,
                    &lease.owner,
                    checkpoint_id,
                    now_ms,
                )
                .await
            }
            None => {
                coverage_gate::bind_import_turn_claim_attempt_with_conn(
                    &txn,
                    &request.session_id,
                    claim,
                    &lease.owner,
                    checkpoint_id,
                    now_ms,
                )
                .await
            }
        },
        ImportAttemptBinding::Retention(claim) => {
            claim
                .verify_current_with_conn(&txn, &request.session_id)
                .await?;
            Ok(true)
        }
    }
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
    #[cfg(test)]
    import_test_failpoint("before_finalize_identity")?;
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
                contiguous_next_ordinal(&request.turns.iter().map(|turn| turn.ordinal).collect())?
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
    let bytes = serde_json::to_vec_pretty(&normalize_json_value(value.clone()))
        .context("serialize safe import projection")?;
    Ok(Redactor::new_default().redact(&bytes).0)
}

/// Full diagnostics have one content-addressed report blob. Per-checkpoint
/// metadata contains only a bounded summary of a Codex session's turn rows.
struct PreparedImportReports {
    full: RedactedBytes,
    metadata: serde_json::Value,
    /// Pi only: the report without its retained thinking, and the retained
    /// blocks by the turn that holds them. Each turn's checkpoint stores only
    /// its own blocks; a block before the first prompt goes with the first
    /// turn. Metadata and the session-wide report keep only their count.
    pi_turns: Option<(serde_json::Value, BTreeMap<String, Vec<serde_json::Value>>)>,
}

impl PreparedImportReports {
    fn new(request: &ImportRequest) -> Result<Self> {
        let mut report = request.redaction_report.clone();
        let mut pi_turns = None;
        if request.agent_kind == AgentKind::Pi
            && let Some(serde_json::Value::Array(blocks)) = report
                .as_object_mut()
                .and_then(|report| report.remove("pi_retained"))
        {
            let first_turn = request
                .turns
                .iter()
                .min_by_key(|turn| turn.ordinal)
                .map(|turn| turn.logical_turn_key.clone())
                .unwrap_or_default();
            // One set of the turn keys, so grouping stays linear in the
            // blocks however many turns the session has.
            let turn_keys: BTreeSet<&str> = request
                .turns
                .iter()
                .map(|turn| turn.logical_turn_key.as_str())
                .collect();
            let mut by_turn: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
            let count = blocks.len();
            for block in blocks {
                let key = block
                    .get("turn_key")
                    .and_then(serde_json::Value::as_str)
                    .filter(|key| turn_keys.contains(key))
                    .map_or_else(|| first_turn.clone(), str::to_owned);
                by_turn.entry(key).or_default().push(block);
            }
            report["pi_retained_blocks"] = serde_json::json!(count);
            pi_turns = Some((report.clone(), by_turn));
        }
        Ok(Self {
            full: redacted_json(&report)?,
            metadata: checkpoint_redaction_metadata(request.agent_kind, &report),
            pi_turns,
        })
    }

    /// The report a turn's checkpoint stores: for Pi, the session report plus
    /// that turn's retained thinking blocks only.
    fn full_for_turn(&self, logical_turn_key: &str) -> Result<RedactedBytes> {
        let Some((report, by_turn)) = &self.pi_turns else {
            return Ok(self.full.clone());
        };
        let mut report = report.clone();
        report["pi_retained"] =
            serde_json::Value::Array(by_turn.get(logical_turn_key).cloned().unwrap_or_default());
        redacted_json(&report)
    }
}

fn checkpoint_redaction_metadata(kind: AgentKind, report: &serde_json::Value) -> serde_json::Value {
    let mut metadata = report.clone();
    if kind != AgentKind::Codex {
        return metadata;
    }
    let Some(stats) = report.get("codex_reasoning") else {
        return metadata;
    };
    let rows = stats.get("turns").and_then(serde_json::Value::as_array);
    let mut counts = serde_json::Map::new();
    for field in [
        "readable_parts",
        "readable_utf8_bytes",
        "empty_parts",
        "opaque_nonempty",
        "encrypted_empty",
        "encrypted_null",
        "encrypted_missing",
        "unproven_fields",
        "unsupported_records",
    ] {
        let total = rows
            .into_iter()
            .flatten()
            .filter_map(|row| {
                row.get("counts")
                    .and_then(|counts| counts.get(field))
                    .and_then(serde_json::Value::as_u64)
            })
            .fold(0u64, u64::saturating_add);
        counts.insert(field.to_owned(), total.into());
    }
    metadata["codex_reasoning"] = serde_json::json!({
        "schema_version":1,"source_status":"unproven",
        "partial":stats.get("partial").and_then(serde_json::Value::as_bool).unwrap_or(true),
        "statistics_unavailable":stats.get("statistics_unavailable").and_then(serde_json::Value::as_bool).unwrap_or(true),
        "retention_unavailable":stats.get("retention_unavailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        "has_unproven_data":stats.get("has_unproven_data").and_then(serde_json::Value::as_bool).unwrap_or(false),
        "provider_turn_count":rows.map(Vec::len),"counts":counts,
        "turn_attribution_unproven":stats.get("turn_attribution_unproven").and_then(serde_json::Value::as_bool).unwrap_or(false),
    });
    metadata
}

fn redacted_json_line(value: &serde_json::Value) -> Result<RedactedBytes> {
    let mut bytes = serde_json::to_vec(&normalize_json_value(value.clone()))
        .context("serialize safe import JSONL record")?;
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
    committed_source_ordinals_for_coverage(conn, request, None, deadline).await
}

async fn committed_source_ordinals_for_coverage(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    versions: Option<&BTreeMap<String, CoverageSchemaVersion>>,
    deadline: Instant,
) -> Result<BTreeSet<usize>> {
    let mut ordinals = BTreeMap::<usize, bool>::new();
    for batch in request.turns.chunks(64) {
        let mut requested = Vec::with_capacity(batch.len());
        let mut values = Vec::<Value>::with_capacity(batch.len() * 2 + 1);
        for turn in batch {
            let version = match versions {
                Some(versions) => *versions
                    .get(&turn.logical_turn_key)
                    .context("import cursor lost its coverage version binding")?,
                None => CoverageSchemaVersion::V1,
            };
            requested.push((turn, version));
            values.push(turn.logical_turn_key.clone().into());
            values.push(version.as_i64().into());
        }
        values.push(request.session_id.clone().into());
        // At most 64 exact indexed tuples / 129 bindings. Do not load the
        // session's historical claims or issue one round trip per turn.
        let placeholders = vec!["(?, ?)"; batch.len()].join(", ");
        let sql = format!(
            "WITH requested(logical_turn_key, coverage_schema_version) AS (VALUES {placeholders})
            SELECT c.logical_turn_key, c.coverage_schema_version
            FROM requested r JOIN agent_coverage_claim c
              ON c.session_id = ? AND c.logical_turn_key = r.logical_turn_key
                AND c.coverage_schema_version = r.coverage_schema_version
            WHERE c.state = 'catalog_committed'"
        );
        let rows = await_import_precommit_read_until(deadline, async {
            conn.query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                sql,
                values,
            ))
            .await
            .context("load committed source tuples for import cursor")
        })
        .await?;
        let committed = rows
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get_by::<String, _>("logical_turn_key")?,
                    row.try_get_by::<i64, _>("coverage_schema_version")?,
                ))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        for (turn, version) in requested {
            *ordinals.entry(turn.ordinal).or_insert(true) &=
                committed.contains(&(turn.logical_turn_key.clone(), version.as_i64()));
        }
    }
    Ok(ordinals
        .into_iter()
        .filter_map(|(ordinal, committed)| committed.then_some(ordinal))
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

fn import_error_with_child_progress(
    error: anyhow::Error,
    request: &ImportRequest,
    lease: &ImportLease,
    outcome: &coverage_gate::LiveReservationOutcome,
    written: usize,
    child_written: usize,
) -> anyhow::Error {
    if child_written == 0 && written == 0 {
        return error;
    }
    let previous = error.downcast_ref::<ImportProgressError>();
    let written = previous.map_or(written, |progress| {
        written.max(progress.summary.checkpoints_written)
    });
    let child_written = previous.map_or(child_written, |progress| {
        child_written.max(progress.subagent_checkpoints_written)
    });
    error.context(ImportProgressError {
        summary: ImportSummary {
            session_id: request.session_id.clone(),
            agent_kind: request.agent_kind.as_db_str().to_owned(),
            turns_seen: request.turns.len(),
            checkpoints_written: written,
            skipped_covered: outcome.skipped_covered,
            skipped_inflight: outcome.skipped_inflight,
            conflicted: outcome.conflicted,
            partial: true,
        },
        subagent_checkpoints_written: child_written,
        import_identity_id: lease.identity_id.clone(),
        import_fence_token: lease.fence_token,
    })
}

async fn capture_imported_subagent_content(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    storage_root: &std::path::Path,
    request: &ImportRequest,
    discovery: &super::subagent_content::SubagentDiscovery,
    deadline: CaptureCommitDeadline,
) -> Result<super::subagent_content::SubagentCaptureSummary> {
    // Sources are constructed only by the Root discovery owner. The content
    // writer verifies their provider, parent identity and scope independently.
    if discovery.warning.is_some() {
        tracing::warn!(
            reason = "subagent_discovery_unavailable",
            "historical import subagent discovery is unavailable; parent import remains valid"
        );
    }
    if discovery.sources.is_empty() {
        return Ok(super::subagent_content::SubagentCaptureSummary::default()
            .with_discovery_usage(discovery));
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
    Ok(summary.with_discovery_usage(discovery))
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

fn historical_child_capture_is_partial(
    kind: AgentKind,
    summary: &super::subagent_content::SubagentCaptureSummary,
) -> bool {
    summary.partial_sources > 0 || (kind == AgentKind::Codex && summary.skipped_inflight > 0)
}

/// The agent kinds a historical import may store: the single source for the
/// CLI `agent import --agent` parser and every prepared-import entry point
/// (PI-29). Readability alone does not make a kind importable.
pub const IMPORTABLE_AGENT_KINDS: &[AgentKind] = &[
    AgentKind::ClaudeCode,
    AgentKind::Codex,
    AgentKind::OpenCode,
    AgentKind::Hermes,
    AgentKind::Pi,
];

/// Whether a historical import may store sessions of `kind`.
pub fn is_importable_kind(kind: AgentKind) -> bool {
    IMPORTABLE_AGENT_KINDS.contains(&kind)
}

/// Refuse a kind outside [`IMPORTABLE_AGENT_KINDS`] before any lease, catalog
/// row, checkpoint, or object is written, so an embedding caller cannot
/// store a session the CLI would refuse.
fn refuse_unimportable_kind(kind: AgentKind) -> Result<()> {
    if is_importable_kind(kind) {
        return Ok(());
    }
    bail!(
        "agent kind '{}' cannot be imported (prepared imports accept claude_code, codex, opencode, hermes, or pi); nothing was written",
        kind.as_db_str()
    )
}

/// Persist one prepared source with per-turn checkpoints and the shared
/// claim/ref/catalog/identity transaction.
pub async fn import_prepared(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
) -> Result<ImportSummary> {
    refuse_unimportable_kind(request.agent_kind)?;
    let monotonic_deadline = deadline.monotonic();
    let binding = LiveCaptureBinding::for_stored(request.agent_kind);
    let discovery = if let Some(port) = binding.subagent_discovery() {
        let discovery_deadline =
            super::subagent_content::discovery_deadline_preserving_parent(monotonic_deadline)?;
        let context = LiveCaptureContext {
            libra_session_id: &request.session_id,
            provider_session_id: &request.provider_session_id,
            verified_cwd: &request.working_dir,
        };
        match port
            .discover_stored(
                &context,
                StoredSubagentSelection::ImportCurrent {
                    parent_locator: None,
                },
                SubagentDiscoveryLimits {
                    byte_budget: super::observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES,
                    source_limit: super::subagent_content::MAX_SUBAGENT_SOURCES_PER_CAPTURE,
                },
                discovery_deadline,
            )
            .await
        {
            Ok(discovery) => discovery,
            Err(_) if request.agent_kind == AgentKind::Codex => {
                super::subagent_content::SubagentDiscovery::charged_stored_unavailable(
                    super::observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES,
                )
            }
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
    import_prepared_with_subagent_discovery(conn, storage_root, request, deadline, discovery, None)
        .await
        .map(|detailed| detailed.summary)
}

/// Import already prepared, redacted ordinary turns with explicit coverage
/// versions. `versions[i]` describes `request.turns[i]`; this does not authorize
/// native reasoning or activate version selection in CLI/provider capture.
///
/// Separate requests may explicitly use independent v1/v2 claims for the same
/// key. A single request must use distinct keys and source ordinals. The import
/// identity records the latest request digest/cursor; claims remain the durable
/// authority for idempotence across alternating request versions.
pub async fn import_prepared_with_coverage_versions(
    conn: &DatabaseConnection,
    storage_root: &Path,
    mut request: ImportRequest,
    versions: &[i64],
    budget: Duration,
    helper_program: Option<&Path>,
) -> Result<ImportSummary> {
    refuse_unimportable_kind(request.agent_kind)?;
    if budget.is_zero() || budget > Duration::from_secs(300) {
        bail!("explicit import budget must be greater than zero and at most 300 seconds");
    }
    let deadline = CaptureCommitDeadline::from_budget(budget)?;
    validate_direct_import_turn_bounds(&request.turns, deadline.monotonic())?;
    if versions.len() != request.turns.len() || request.turns.is_empty() {
        bail!("explicit import requires one coverage version for every nonempty source turn");
    }
    let mut bound_versions = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for (turn, version) in request.turns.iter().zip(versions) {
        ensure_before_deadline(deadline.monotonic())?;
        let version = CoverageSchemaVersion::try_from(*version)?;
        if bound_versions
            .insert(turn.logical_turn_key.clone(), version)
            .is_some()
            || !ordinals.insert(turn.ordinal)
            || !request.turn_boundaries.contains_key(&turn.logical_turn_key)
        {
            bail!(
                "explicit import requires distinct logical turn keys and ordinals with matching chronology"
            );
        }
    }
    if request.turn_boundaries.len() != request.turns.len() {
        bail!("explicit import chronology differs from its source turns");
    }
    // A prepared embedding request is not a source/provenance capability.
    // Refuse any newly matched content before digest, lease or persistence.
    if redact_turns_with_report(&mut request.turns).match_count() != 0 {
        return Err(ImportError::SourceAuthorization.into());
    }
    request.content_digest = source_digest_for_coverage_versions(&request.turns, versions)?;
    validate_persisted_import_timestamps(&request, Utc::now().timestamp())?;
    assert_v2_durable_import_request(&request)?;
    if request.session_id
        != build_ai_session_id(
            capture_provider_name(request.agent_kind),
            &request.provider_session_id,
        )
    {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    let scope = capture_scope_for_request_until(conn, &request, deadline.monotonic()).await?;
    validate_import_storage_and_scope_until(conn, storage_root, &scope, deadline.monotonic())
        .await?;
    let existing = await_import_precommit_read_until(
        deadline.monotonic(),
        load_existing_session_ownership_in_scope(
            conn,
            request.agent_kind,
            &request.provider_session_id,
            &scope,
        ),
    )
    .await?;
    validate_scoped_prepared_existing_session(&mut request, existing.as_ref())?;
    super::authorized_read::with_explicit_import_checkpoint_helper(
        helper_program,
        deadline.monotonic(),
        import_prepared_with_reasoning_identity_batches_and_coverage(
            conn,
            storage_root,
            request,
            deadline,
            super::subagent_content::SubagentDiscovery::default(),
            None,
            BTreeMap::new(),
            Some(bound_versions),
            None,
            None,
        ),
    )
    .await
    .map(|detailed| detailed.summary)
}

async fn validate_import_storage_and_scope_until(
    conn: &DatabaseConnection,
    storage_root: &Path,
    scope: &CaptureScope,
    deadline: Instant,
) -> Result<()> {
    let database_scope =
        await_import_precommit_read_until(deadline, CaptureScope::main_for_connection(conn))
            .await?;
    if scope.repo_id != database_scope.repo_id {
        return Err(ImportError::RepositoryConflict.into());
    }
    if !storage_root.is_absolute() {
        bail!("explicit import requires the database's absolute .libra storage directory");
    }
    let databases = await_import_precommit_read_until(deadline, async {
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA database_list".to_owned(),
        ))
        .await
        .context("verify the explicit import SQLite repository database")
    })
    .await?;
    let mut database_file = None;
    for database in databases {
        if database.try_get_by::<String, _>("name")? == "main" {
            database_file = Some(database.try_get_by::<String, _>("file")?);
        }
    }
    let expected_database = storage_root.join(util::DATABASE);
    if database_file
        .as_ref()
        .is_none_or(|file| Path::new(file) != expected_database)
    {
        bail!(
            "explicit import database and storage root differ; pass the database's absolute .libra directory"
        );
    }
    Ok(())
}

/// Check sizes/depth before redaction or canonical encoding can allocate a
/// second transcript-sized buffer. Only the four ordinary record kinds enter
/// this prepared-import seam; sealed reasoning stays with its existing owner.
fn validate_direct_import_turn_bounds(turns: &[NormalizedTurn], deadline: Instant) -> Result<()> {
    validate_import_turn_bounds(turns, deadline, false)
}

fn validate_import_turn_bounds(
    turns: &[NormalizedTurn],
    deadline: Instant,
    runtime: bool,
) -> Result<()> {
    use super::observed_agents::SemanticRecord;
    if turns.len() > 4096 {
        return Err(ImportError::BatchInputLimit.into());
    }
    let mut remaining = if runtime {
        40 * 1024 * 1024
    } else {
        usize::try_from(TRANSCRIPT_READ_HARD_CAP_BYTES)?
    };
    let mut nodes = 0usize;
    let mut charge = |bytes: usize| -> Result<()> {
        if runtime {
            nodes = nodes
                .checked_add(1)
                .filter(|nodes| *nodes <= 65_536)
                .ok_or(ImportError::BatchInputLimit)?;
        }
        remaining = remaining
            .checked_sub(bytes)
            .ok_or(ImportError::BatchInputLimit)?;
        Ok(())
    };
    let string_cost = |value: &str| {
        value
            .len()
            .checked_mul(if runtime { 1 } else { 6 })
            .and_then(|n| n.checked_add(8))
            .ok_or(ImportError::BatchInputLimit)
    };
    for turn in turns {
        ensure_before_deadline(deadline)?;
        charge(128)?;
        charge(string_cost(&turn.logical_turn_key)?)?;
        for record in &turn.records {
            charge(64)?;
            match record {
                SemanticRecord::User { text } | SemanticRecord::Assistant { text } => {
                    charge(string_cost(text)?)?
                }
                SemanticRecord::ToolResult {
                    call_id, content, ..
                } => {
                    if let Some(id) = call_id {
                        charge(string_cost(id)?)?;
                    }
                    charge(string_cost(content)?)?;
                }
                SemanticRecord::ToolCall {
                    call_id,
                    name,
                    input,
                } => {
                    if let Some(id) = call_id {
                        charge(string_cost(id)?)?;
                    }
                    charge(string_cost(name)?)?;
                    let mut stack = vec![(input, 0usize)];
                    while let Some((value, depth)) = stack.pop() {
                        ensure_before_deadline(deadline)?;
                        if depth > 64 {
                            return Err(ImportError::BatchInputLimit.into());
                        }
                        charge(32)?;
                        match value {
                            CanonValue::Str(text) => charge(string_cost(text)?)?,
                            CanonValue::Array(values) => {
                                charge(
                                    values
                                        .len()
                                        .checked_mul(16)
                                        .ok_or(ImportError::BatchInputLimit)?,
                                )?;
                                stack.extend(values.iter().map(|value| (value, depth + 1)));
                            }
                            CanonValue::Object(values) => {
                                charge(
                                    values
                                        .len()
                                        .checked_mul(16)
                                        .ok_or(ImportError::BatchInputLimit)?,
                                )?;
                                for (key, value) in values {
                                    charge(string_cost(key)?)?;
                                    stack.push((value, depth + 1));
                                }
                            }
                            CanonValue::Float(_) => bail!(
                                "explicit import tool input must use normalized integer semantic values"
                            ),
                            CanonValue::Null | CanonValue::Bool(_) | CanonValue::Int(_) => {}
                        }
                    }
                }
                SemanticRecord::Reasoning { text, .. } if runtime => {
                    charge(string_cost(text.as_str())?)?
                }
                SemanticRecord::Reasoning { .. } => bail!(
                    "explicit ordinary import does not authorize reasoning; use the typed capture source"
                ),
            }
        }
    }
    Ok(())
}

/// A supplement preserves newly available ordinary source values without
/// changing a committed semantic claim or authenticating native reasoning.
struct RetentionSupplementContext<'a> {
    conn: &'a DatabaseConnection,
    storage: &'a Path,
    request: &'a ImportRequest,
    scope: &'a CaptureScope,
    lease: &'a ImportLease,
    deadline: CaptureCommitDeadline,
    reports: &'a PreparedImportReports,
    reservation: &'a coverage_gate::LiveReservationOutcome,
    usage_attribution: Option<&'a serde_json::Value>,
}

struct RetentionSupplementCommit<'a> {
    source: &'a coverage_gate::CommittedTurnClaimSnapshot,
    ordinary: LiveClaimCommitPlan,
}

#[async_trait::async_trait]
impl super::traces::TracesTxnExtra for RetentionSupplementCommit<'_> {
    async fn apply(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        ctx: &super::traces::TracesCommitCtx,
    ) -> Result<()> {
        self.source
            .verify_current_with_conn(txn, &self.ordinary.session_id)
            .await?;
        super::traces::TracesTxnExtra::apply(&self.ordinary, txn, ctx).await
    }
}

#[derive(Clone, Copy)]
enum RetentionLookup {
    Present,
    Missing,
    Unavailable,
}

/// Existing ordinary bytes are only a deduplication optimization. If a legacy
/// layout or packed object cannot be inspected, save a fresh authorized copy;
/// never mistake a failed read for evidence that retention is already durable.
async fn read_checkpoint_retention(
    context: &RetentionSupplementContext<'_>,
    checkpoint_id: &str,
    expected: &[u8],
) -> Result<RetentionLookup> {
    use super::checkpoint_reader::{
        checkpoint_plain_files, checkpoint_plain_manifest, checkpoint_transcript_oids,
        read_checkpoint_object_bounded, read_tree_object, reasoning_artifact_tree_oids, subtree,
    };
    ensure_before_deadline(context.deadline.monotonic())?;
    let row =
        await_import_precommit_read_until(context.deadline.monotonic(), async {
            context.conn.query_one_raw(Statement::from_sql_and_values(
            context.conn.get_database_backend(),
            "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ? AND session_id = ?",
            [checkpoint_id.into(), context.request.session_id.clone().into()],
        )).await.context("read ordinary retention checkpoint catalog")
        })
        .await?;
    let Some(row) = row else {
        return Ok(RetentionLookup::Missing);
    };
    let read = (|| -> Result<bool> {
        let tree_oid: String = row
            .try_get_by("tree_oid")
            .context("decode ordinary retention checkpoint tree")?;
        let invalid = || {
            anyhow!("ordinary retention checkpoint cannot be verified; run `libra agent doctor`")
        };
        let root = read_tree_object(context.storage, &tree_oid).map_err(|_| invalid())?;
        let checkpoints = subtree(context.storage, &root, "checkpoint").map_err(|_| invalid())?;
        let prefix = checkpoint_id.get(..2).ok_or_else(invalid)?;
        let suffix = checkpoint_id.get(2..).ok_or_else(invalid)?;
        let bucket = subtree(context.storage, &checkpoints, prefix).map_err(|_| invalid())?;
        let inner = subtree(context.storage, &bucket, suffix).map_err(|_| invalid())?;
        let artifacts =
            reasoning_artifact_tree_oids(context.storage, &inner).map_err(|_| invalid())?;
        let files =
            checkpoint_plain_files(context.storage, &inner, &artifacts).map_err(|_| invalid())?;
        let manifest = checkpoint_plain_manifest(context.storage, &inner, &artifacts, &files)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        let oids = checkpoint_transcript_oids(context.storage, &inner, &manifest, &artifacts)
            .map_err(|_| invalid())?;
        let cap = TRANSCRIPT_READ_HARD_CAP_BYTES.saturating_mul(2);
        let mut bytes = Vec::new();
        for oid in oids {
            ensure_before_deadline(context.deadline.monotonic())?;
            let remaining = cap.saturating_sub(bytes.len() as u64);
            let (part, truncated) =
                read_checkpoint_object_bounded(context.storage, &oid, remaining)
                    .map_err(|_| invalid())?;
            if truncated {
                return Err(invalid());
            }
            bytes.extend_from_slice(&part);
        }
        ensure_before_deadline(context.deadline.monotonic())?;
        let Some(position) = bytes.iter().position(|byte| *byte == b'\n') else {
            return Ok(false);
        };
        // INVARIANT: position was found within bytes above.
        let (first, remainder) = bytes.split_at(position);
        let tail = remainder.strip_prefix(b"\n").ok_or_else(invalid)?;
        let Ok(projection) = parse_canon_value(first) else {
            return Ok(false);
        };
        Ok(
            projection.get("agent_kind").and_then(CanonValue::as_str) == Some("codex")
                && matches!(projection.get("schema_version"), Some(CanonValue::Int(1)))
                && tail == expected,
        )
    })();
    ensure_before_deadline(context.deadline.monotonic())?;
    Ok(match read {
        Ok(true) => RetentionLookup::Present,
        Ok(false) => RetentionLookup::Missing,
        Err(_) => RetentionLookup::Unavailable,
    })
}

/// The deterministic ID is keyed by repository/session/turn/content. A
/// committed catalog row proves this exact ordinary copy was persisted, even
/// after its objects have moved into a pack. It grants no native authority.
async fn retention_supplement_exists(
    context: &RetentionSupplementContext<'_>,
    checkpoint_id: &str,
) -> Result<bool> {
    await_import_precommit_read_until(context.deadline.monotonic(), async {
        context.conn.query_one_raw(Statement::from_sql_and_values(
            context.conn.get_database_backend(),
            "SELECT 1 FROM agent_checkpoint WHERE checkpoint_id = ? AND session_id = ? AND scope = 'committed'",
            [checkpoint_id.into(), context.request.session_id.clone().into()],
        )).await.context("read committed ordinary retention supplement")
    }).await.map(|row| row.is_some())
}

#[derive(Default)]
struct RetentionSupplementProgress {
    written: usize,
    pending: bool,
}

/// One ordinary content identity for fresh and supplemental copies. Include
/// the V1 semantic bytes and completeness so a later semantic revision cannot
/// reuse an earlier checkpoint. This is a keyed data ID, never native proof.
async fn codex_retention_checkpoint_id(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    storage: &Path,
    request: &ImportRequest,
    turn: &NormalizedTurn,
    retained: &RedactedBytes,
    deadline: CaptureCommitDeadline,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"libra-codex-ordinary-retention-v1\0");
    let semantic_digest = turn.digest_hex();
    for value in [
        request.session_id.as_bytes(),
        turn.logical_turn_key.as_bytes(),
        b"1",
        semantic_digest.as_bytes(),
        turn.completeness.as_db_str().as_bytes(),
        retained.bytes(),
    ] {
        update_length_delimited_import_preimage(&mut digest, value)?;
    }
    let preimage: [u8; 32] = digest.finalize().into();
    let commitment = super::capture::key::derive_snapshot_content_commitment_in_scope_until(
        conn,
        scope,
        storage,
        &request.working_dir,
        &preimage,
        deadline.monotonic(),
    )
    .await?;
    Ok(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, commitment.as_bytes()).to_string())
}

async fn append_codex_retention_supplements(
    context: RetentionSupplementContext<'_>,
    retention: &CodexRetention,
    covered: &[coverage_gate::CommittedTurnClaimSnapshot],
) -> Result<RetentionSupplementProgress> {
    let mut written = 0;
    for source in covered {
        if written >= 16
            && retention
                .for_turn(source.replay_fence().logical_turn_key)
                .is_some()
        {
            return Ok(RetentionSupplementProgress {
                written,
                pending: true,
            });
        }
        let mut marker_fence = None;
        let result: Result<bool> = async {
        ensure_before_deadline(context.deadline.monotonic())?;
        let key = source.replay_fence().logical_turn_key;
        let Some(retained) = retention.for_turn(key) else {
            return Ok(false);
        };
        source
            .verify_current_with_conn(context.conn, &context.request.session_id)
            .await?;
        let turn = context.request.turns.iter().find(|turn| turn.logical_turn_key == key)
            .context("ordinary retention source has no corresponding semantic turn")?;
        let checkpoint_id = codex_retention_checkpoint_id(context.conn, context.scope, context.storage,
            context.request, turn, retained, context.deadline).await?;
        if retention_supplement_exists(&context, &checkpoint_id).await? {
            source.verify_current_with_conn(context.conn, &context.request.session_id).await?;
            return Ok(false);
        }
        let prior_retention_unavailable = match read_checkpoint_retention(&context, source.checkpoint_id(), retained.bytes()).await? {
            RetentionLookup::Present => {
                source.verify_current_with_conn(context.conn, &context.request.session_id).await?;
                return Ok(false);
            }
            RetentionLookup::Missing => false,
            RetentionLookup::Unavailable => true,
        };
        let now_ms = Utc::now().timestamp_millis();
        let marker = bind_import_attempt(
            context.conn,
            context.request,
            context.lease,
            ImportAttemptBinding::Retention(source),
            &checkpoint_id,
            now_ms,
            context.deadline,
        )
        .await?;
        let generation = marker
            .generation
            .clone()
            .context("ordinary retention attempt has no marker generation")?;
        marker_fence = Some((checkpoint_id.clone(), generation));
            let writer = ImportedCheckpointWriter::from_prebound_attempt(context.conn, context.storage, marker)
                .map_err(|_| anyhow!("cannot adopt ordinary retention checkpoint attempt; run `libra agent doctor`"))?
                .with_capture_scope(context.scope.clone());
            let turn = context.request.turns.iter().find(|turn| turn.logical_turn_key == key)
                .context("ordinary retention source has no corresponding semantic turn")?;
            let projection = safe_turn_projection("codex", turn);
            let mut transcript = redacted_json_line(&projection)?.into_inner();
            transcript.extend_from_slice(retained.bytes());
            let source_snapshot = context.request.transcript_snapshot.clone().ok_or(ImportError::SourceAuthorization)?;
            let mut metadata = serde_json::json!({
                "schema_version":history::CHECKPOINT_METADATA_SCHEMA_VERSION,
                "checkpoint_id":checkpoint_id,"session_id":context.request.session_id,"agent_kind":"codex",
                "scope":"committed","model":"unknown","provider_session_id":context.request.provider_session_id,
                "created_at":context.request.ended_at,"transcript_snapshot":source_snapshot,
                "redaction_report":context.reports.metadata,
                "import":{"source_kind":context.request.source_kind,"source_id":context.request.source_id,
                    "logical_turn_key":key,"ordinary_retention_supplement":true,
                    "prior_retention_unavailable":prior_retention_unavailable}
            });
            if let Some(extraction) = retention.summary_metadata() {
                metadata["extraction"] = extraction;
                if let Some(attribution) = context.usage_attribution {
                    metadata["usage_attribution"] = attribution.clone();
                }
            }
            let metadata = redacted_json(&metadata)?;
            let event = LifecycleEvent {
                kind: LifecycleEventKind::TurnEnd,
                session_id: context.request.provider_session_id.clone(), session_ref: None,
                prompt: None, model: None,
                source: Some(serde_json::json!({"channel":"import","ordinary_retention_supplement":true})),
                tool_name: None, tool_input: None, tool_response: None, assistant_message: None,
                timestamp: DateTime::<Utc>::from_timestamp(context.request.ended_at, 0)
                    .context("ordinary retention lifecycle timestamp is out of range")?,
            };
            let event_context = CanonicalEventContext {
                agent_kind: "codex", session_id: &context.request.session_id,
                provider_session_id: &context.request.provider_session_id,
                identity_scheme: LifecycleIdentityScheme::ImportUuidV5,
                provenance: serde_json::json!({"channel":"import","ordinary_retention_supplement":true}),
            };
            let lifecycle = redacted_json_line(&lifecycle_event_canonical_json_with_identity(
                &event, &event_context,
                import_lifecycle_event_id(&context.request.session_id, &format!("retention:{checkpoint_id}")),
                true,
            ))?;
            let payload = CheckpointRedactedPayload::from_derived_turn_projection(
                RedactedBytes::new_unchecked(transcript), source_snapshot, metadata,
                lifecycle,
                context.reports.full.clone(),
            ).map_err(|_| anyhow!("cannot prepare ordinary retention checkpoint payload"))?;
            let parent = current_parent_commit(context.conn, context.deadline.monotonic()).await?;
            let ordinals = committed_source_ordinals(context.conn, context.request, context.deadline.monotonic()).await?;
            let plan = RetentionSupplementCommit {
                source,
                ordinary: LiveClaimCommitPlan {
                    source_channel:"import", session_id:context.request.session_id.clone(), checkpoint_id:checkpoint_id.clone(),
                    owner:context.lease.owner.clone(),parent_commit:parent.clone(),created_at:context.request.ended_at,now_ms,
                    claims:Vec::new(),import_session:Some(import_session_commit(context.request, context.scope)?),
                    import_identity:Some(ImportIdentityCommit {
                        identity_id:context.lease.identity_id.clone(),observed_digest:context.request.content_digest.clone(),
                        owner:context.lease.owner.clone(),fence_token:context.lease.fence_token,
                        next_ordinal:contiguous_next_ordinal(&ordinals)?,final_turn:false,
                    }), capture_scope:Some(context.scope.clone()),
                },
            };
            let request = ImportedCheckpointWriteRequest::new(&checkpoint_id,&context.request.session_id,"codex",parent.as_deref(),&payload,&plan,context.deadline)
                .map_err(|_| anyhow!("cannot prepare ordinary retention checkpoint request"))?;
            match writer.write_turn(request).await.map_err(|_| anyhow!("cannot persist ordinary retention checkpoint; run `libra agent doctor`"))? {
                CheckpointWriteOutcome::Written { .. } | CheckpointWriteOutcome::AlreadyExists { .. } | CheckpointWriteOutcome::PendingCleanup { .. } => Ok(true),
                _ => bail!("ordinary retention checkpoint did not commit; retry after the repository writer finishes"),
            }
        }.await;
        if let Err(error) = result {
            let error = abandon_import_attempt_after_error(
                context.conn,
                context.request,
                context.lease,
                marker_fence
                    .as_ref()
                    .map(|(id, generation)| (id.as_str(), generation.as_str())),
                "partial",
                error,
                context.deadline,
            )
            .await;
            if written == 0 {
                return Err(error);
            }
            return Err(ImportProgressError {
                summary: ImportSummary {
                    session_id: context.request.session_id.clone(),
                    agent_kind: context.request.agent_kind.as_db_str().to_owned(),
                    turns_seen: context.request.turns.len(),
                    checkpoints_written: written,
                    skipped_covered: context.reservation.skipped_covered,
                    skipped_inflight: context.reservation.skipped_inflight,
                    conflicted: context.reservation.conflicted,
                    partial: true,
                },
                subagent_checkpoints_written: 0,
                import_identity_id: context.lease.identity_id.clone(),
                import_fence_token: context.lease.fence_token,
            }
            .into());
        }
        if result? {
            written += 1;
        }
    }
    Ok(RetentionSupplementProgress {
        written,
        pending: false,
    })
}

/// Command-path variant whose child discovery was already charged to the
/// batch input budget before any persistence begins.
pub(crate) async fn import_prepared_with_subagent_discovery(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    native_reasoning: Option<NativeProvenReasoning>,
) -> Result<DetailedImportSummary> {
    import_prepared_with_subagent_discovery_and_runtime(
        conn,
        storage_root,
        request,
        deadline,
        subagent_discovery,
        native_reasoning,
        None,
    )
    .await
}

/// Actual descriptor-helper handoff. Runtime data travels separately from
/// public prepared requests and never grants a source/native capability.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_prepared_with_subagent_discovery_and_runtime(
    conn: &DatabaseConnection,
    storage_root: &Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    native_reasoning: Option<NativeProvenReasoning>,
    coverage_runtime: Option<CaptureCoverageRuntime>,
) -> Result<DetailedImportSummary> {
    import_prepared_with_retention(
        conn,
        storage_root,
        request,
        deadline,
        subagent_discovery,
        native_reasoning,
        coverage_runtime,
        None,
    )
    .await
}

/// Ordinary retained source is distinct from the typed semantic/native seam.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_prepared_with_retention(
    conn: &DatabaseConnection,
    storage_root: &Path,
    mut request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    native_reasoning: Option<NativeProvenReasoning>,
    coverage_runtime: Option<CaptureCoverageRuntime>,
    codex_retention: Option<CodexRetention>,
) -> Result<DetailedImportSummary> {
    refuse_unimportable_kind(request.agent_kind)?;
    if codex_retention.is_some()
        && (request.agent_kind != AgentKind::Codex
            || native_reasoning.is_some()
            || coverage_runtime.is_some())
    {
        return Err(ImportError::SourceAuthorization.into());
    }
    let mut versions = None;
    let runtime_source_rejected = coverage_runtime
        .as_ref()
        .map(|runtime| runtime.classifier_rejected);
    if let Some(runtime) = coverage_runtime {
        ensure_before_deadline(deadline.monotonic())?;
        validate_persisted_import_timestamps(&request, Utc::now().timestamp())?;
        assert_v2_durable_import_request(&request)?;
        if request.agent_kind != AgentKind::OpenCode
            || runtime.source_partial
            || (runtime.classifier_rejected && runtime.turns.is_some())
            || request.session_id
                != build_ai_session_id(
                    capture_provider_name(request.agent_kind),
                    &request.provider_session_id,
                )
            || !request
                .transcript_snapshot
                .as_ref()
                .is_some_and(|snapshot| {
                    snapshot.completeness
                        == super::capture::snapshot::CaptureSnapshotCompleteness::Complete
                        && snapshot.source.is_some()
                        && snapshot.has_durable_source_commitment()
                        && snapshot.transcript_redacted_bytes != 0
                })
        {
            return Err(ImportError::SourceAuthorization.into());
        }
        let has_typed_companion = runtime.turns.is_some();
        if let Some(turns) = runtime.turns {
            let active_complete_tail = request.session_state != "stopped"
                && turns
                    .iter()
                    .max_by_key(|turn| turn.ordinal)
                    .is_some_and(|turn| turn.completeness == Completeness::Complete);
            if native_reasoning.is_some() || turns.is_empty() || active_complete_tail {
                return Err(ImportError::SourceAuthorization.into());
            }
            request.turns = turns;
        }
        // Legacy ordinary imports retain their source-bounded turn count.
        // Only the new typed companion owns the stricter runtime map budget.
        if has_typed_companion {
            validate_import_turn_bounds(&request.turns, deadline.monotonic(), true)?;
        }
        let keys = request
            .turns
            .iter()
            .map(|turn| turn.logical_turn_key.as_str())
            .collect::<BTreeSet<_>>();
        let ordinals = request
            .turns
            .iter()
            .map(|turn| turn.ordinal)
            .collect::<BTreeSet<_>>();
        if keys.len() != request.turns.len()
            || ordinals.len() != request.turns.len()
            || keys.len() != request.turn_boundaries.len()
            || request
                .turn_boundaries
                .keys()
                .any(|key| !keys.contains(key.as_str()))
            || redact_turns_with_report(&mut request.turns).match_count() != 0
        {
            return Err(ImportError::SourceAuthorization.into());
        }
        let scope = capture_scope_for_request_until(conn, &request, deadline.monotonic()).await?;
        validate_import_storage_and_scope_until(conn, storage_root, &scope, deadline.monotonic())
            .await?;
        if native_reasoning.is_none() {
            let selected = request
                .turns
                .iter()
                .map(select_coverage_schema)
                .collect::<Vec<_>>();
            request.content_digest = source_digest_for_coverage_versions(
                &request.turns,
                &selected
                    .iter()
                    .map(|version| version.as_i64())
                    .collect::<Vec<_>>(),
            )?;
            versions = Some(
                request
                    .turns
                    .iter()
                    .zip(selected)
                    .map(|(turn, version)| (turn.logical_turn_key.clone(), version))
                    .collect(),
            );
        }
    }
    let (reasoning_identity_plan, per_turn_native) = if let Some(native) = native_reasoning {
        let monotonic_deadline = deadline.monotonic();
        validate_persisted_import_timestamps(&request, Utc::now().timestamp())?;
        assert_v2_durable_import_request(&request)?;
        ensure_before_deadline(monotonic_deadline)?;
        let import_scope =
            capture_scope_for_request_until(conn, &request, monotonic_deadline).await?;
        let expected_session = build_ai_session_id(
            capture_provider_name(request.agent_kind),
            &request.provider_session_id,
        );
        if request.session_id != expected_session {
            return Err(ImportError::SourceAuthorization.into());
        }
        if !request
            .transcript_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.completeness
                    == super::capture::snapshot::CaptureSnapshotCompleteness::Complete
                    && snapshot.source.is_some()
                    && snapshot.has_durable_source_commitment()
                    && snapshot.transcript_redacted_bytes != 0
            })
        {
            return Err(ImportError::SourceAuthorization.into());
        }
        let turn_keys = request
            .turns
            .iter()
            .map(|turn| turn.logical_turn_key.as_str())
            .collect::<BTreeSet<_>>();
        let ordinals = request
            .turns
            .iter()
            .map(|turn| turn.ordinal)
            .collect::<BTreeSet<_>>();
        if turn_keys.len() != request.turns.len()
            || ordinals.len() != request.turns.len()
            || turn_keys.len() != request.turn_boundaries.len()
            || request
                .turn_boundaries
                .keys()
                .any(|key| !turn_keys.contains(key.as_str()))
        {
            return Err(ImportError::SourceAuthorization.into());
        }
        // The authorized helper already redacted these semantic turns and
        // accounted for that scan in its report. Recheck the parent-owned
        // projection before any claim mutation, but never count the same
        // fields twice. Newly matched content means the helper response did
        // not satisfy the prepared-source privacy contract.
        if redact_turns_with_report(&mut request.turns).match_count() != 0 {
            return Err(ImportError::SourceAuthorization.into());
        }
        request.content_digest = source_digest(&request.turns);

        let reasoning = ReasoningSourceDisposition::NativeProven(native);
        let plan = prepare_reasoning_coverage_plan_from_disposition(
            &reasoning,
            ReasoningIdentityPreparation {
                conn,
                scope: &import_scope,
                storage_path: storage_root,
                authorized_root: &request.working_dir,
                session_id: &request.session_id,
                provider: request.agent_kind.as_db_str(),
                turns: &request.turns,
                deadline: monotonic_deadline,
            },
        )
        .await?
        .context("native import source has no authenticated reasoning identity")?;
        let ReasoningSourceDisposition::NativeProven(native) = reasoning else {
            return Err(ImportError::SourceAuthorization.into());
        };
        let per_turn = partition_reasoning_for_import(
            native,
            &request.session_id,
            request.agent_kind.as_db_str(),
            &request.provider_session_id,
            &request.turns,
        )
        .map_err(|error| anyhow!("validate native import turn association: {error}"))?;
        for turn in &request.turns {
            let identity = plan
                .turn(&turn.logical_turn_key)
                .context("native import reasoning identity is missing a normalized logical turn")?;
            let has_native = per_turn.contains_key(&turn.logical_turn_key);
            if (identity.kind() == crate::internal::metadata::ReasoningIdentityKind::Nonempty)
                != has_native
            {
                return Err(ImportError::SourceAuthorization.into());
            }
        }
        (Some(plan), per_turn)
    } else {
        (None, BTreeMap::new())
    };
    import_prepared_with_reasoning_identity_batches_and_coverage(
        conn,
        storage_root,
        request,
        deadline,
        subagent_discovery,
        reasoning_identity_plan,
        per_turn_native,
        versions,
        runtime_source_rejected,
        codex_retention,
    )
    .await
}

/// Parent-owned typed identity handoff for a prepared import. Keeping it
/// outside `ImportRequest` prevents serialized helper projections from
/// asserting native provenance.
#[cfg(all(test, unix))]
pub(crate) async fn import_prepared_with_reasoning_identity(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    reasoning_identity_plan: Option<ReasoningCoveragePlan>,
) -> Result<DetailedImportSummary> {
    import_prepared_with_reasoning_identity_and_batches(
        conn,
        storage_root,
        request,
        deadline,
        subagent_discovery,
        reasoning_identity_plan,
        BTreeMap::new(),
    )
    .await
}

#[cfg(all(test, unix))]
async fn import_prepared_with_reasoning_identity_and_batches(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    reasoning_identity_plan: Option<ReasoningCoveragePlan>,
    per_turn_native: BTreeMap<String, NativeProvenReasoning>,
) -> Result<DetailedImportSummary> {
    import_prepared_with_reasoning_identity_batches_and_coverage(
        conn,
        storage_root,
        request,
        deadline,
        subagent_discovery,
        reasoning_identity_plan,
        per_turn_native,
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn import_prepared_with_reasoning_identity_batches_and_coverage(
    conn: &DatabaseConnection,
    storage_root: &std::path::Path,
    request: ImportRequest,
    deadline: CaptureCommitDeadline,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
    reasoning_identity_plan: Option<ReasoningCoveragePlan>,
    mut per_turn_native: BTreeMap<String, NativeProvenReasoning>,
    coverage_versions: Option<BTreeMap<String, CoverageSchemaVersion>>,
    runtime_source_rejected: Option<bool>,
    codex_retention: Option<CodexRetention>,
) -> Result<DetailedImportSummary> {
    let monotonic_deadline = deadline.monotonic();
    // The shared persistence path repeats the kind gate for crate callers
    // that reach it without a public entry point.
    refuse_unimportable_kind(request.agent_kind)?;
    // A prepared request can cross the private preparation-helper boundary or
    // be supplied by an embedding caller. Revalidate all durable time fields
    // before even resolving a capture scope, let alone acquiring an identity
    // lease or writing a catalog row.
    validate_persisted_import_timestamps(&request, Utc::now().timestamp())?;
    assert_v2_durable_import_request(&request)?;
    ensure_before_deadline(monotonic_deadline)?;
    let reports = PreparedImportReports::new(&request)?;
    ensure_before_deadline(monotonic_deadline)?;
    let import_scope = capture_scope_for_request_until(conn, &request, monotonic_deadline).await?;
    if let Some(reasoning) = reasoning_identity_plan.as_ref() {
        reasoning.validate_for(&import_scope, &request.session_id, &request.turns)?;
    }
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
    let reasoning_reader = super::capture::checkpoint::ReasoningIdentityReadContext::new(
        &import_scope,
        &request.session_id,
        storage_root,
        &request.working_dir,
        monotonic_deadline,
    );
    let reasoning_input = match reasoning_identity_plan.as_ref() {
        Some(reasoning) => ReasoningCoverageInput::native(reasoning),
        None => ReasoningCoverageInput::ordinary(&reasoning_reader),
    };
    let reasoning_input = if codex_retention
        .as_ref()
        .is_some_and(|retention| !retention.is_unavailable())
    {
        reasoning_input.with_ordinary_retention_observations()
    } else {
        reasoning_input
    };
    let reservation = if let Some(rejected) = runtime_source_rejected {
        coverage_gate::reserve_runtime_import_turn_claims_with_reasoning_until(
            conn,
            &import_scope,
            &request.session_id,
            &ordered_owned,
            &lease.owner,
            started_ms,
            deadline,
            reasoning_input,
            rejected,
        )
        .await
    } else {
        match coverage_versions.as_ref() {
            Some(versions) => {
                let ordered_versions = ordered_owned
                    .iter()
                    .map(|turn| {
                        versions
                            .get(&turn.logical_turn_key)
                            .map(|version| version.as_i64())
                            .context("import reservation lost its coverage version binding")
                    })
                    .collect::<Result<Vec<_>>>()?;
                coverage_gate::reserve_import_turn_claims_by_version_until(
                    conn,
                    &import_scope,
                    &request.session_id,
                    &ordered_owned,
                    &ordered_versions,
                    &lease.owner,
                    started_ms,
                    deadline,
                    reasoning_input,
                )
                .await
            }
            None => {
                coverage_gate::reserve_import_turn_claims_with_reasoning_until(
                    conn,
                    &import_scope,
                    &request.session_id,
                    &ordered_owned,
                    &lease.owner,
                    started_ms,
                    deadline,
                    reasoning_input,
                )
                .await
            }
        }
    };
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
    if outcome.skipped_incomplete_reasoning != 0 {
        // This is an observed partial source, not an all-covered import.
        // The reservation owner has released every earlier batch. Do not
        // capture child sources, build parent objects or advance a cursor.
        if let Err(error) = finalize_noop_identity(
            conn,
            &request,
            &lease,
            "partial",
            Some("LBR-AGENT-018"),
            Utc::now().timestamp_millis(),
            deadline,
        )
        .await
        {
            return Err(abandon_import_attempt_after_error(
                conn, &request, &lease, None, "partial", error, deadline,
            )
            .await);
        }
        return Ok(DetailedImportSummary::new(
            ImportSummary {
                session_id: request.session_id,
                agent_kind: request.agent_kind.as_db_str().to_string(),
                turns_seen: request.turns.len(),
                checkpoints_written: 0,
                skipped_covered: outcome.ordinary.skipped_covered,
                skipped_inflight: outcome.ordinary.skipped_inflight,
                conflicted: outcome.ordinary.conflicted,
                partial: true,
            },
            0,
            &lease,
        ));
    }
    let covered = outcome.covered;
    let versioned_reserved = outcome.versioned_reserved;
    let outcome = outcome.ordinary;
    // Reservation is the authority boundary for owned native bytes. Drop
    // covered or conflicted turn batches now; only a fresh reserved claim may
    // carry its corresponding batch to the checkpoint writer.
    let reserved_keys = outcome
        .reserved
        .iter()
        .map(|claim| claim.logical_turn_key.as_str())
        .collect::<BTreeSet<_>>();
    per_turn_native.retain(|key, _| reserved_keys.contains(key.as_str()));
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
    ) || (request.agent_kind == AgentKind::Hermes
        && request
            .turns
            .iter()
            .any(|turn| turn.completeness == Completeness::Incomplete));
    partial |= codex_retention
        .as_ref()
        .is_some_and(CodexRetention::is_unavailable);
    let defer_identity_for_subagents = !subagent_discovery.sources.is_empty();
    // Codex attribution describes this import pass, so its durable child
    // snapshot must exist before either parent metadata construction point.
    // Preserve failures until after the independent parent writes finish.
    let child_deadline = super::subagent_content::import_child_work_deadline(deadline);
    let mut early_child_capture = if request.agent_kind == AgentKind::Codex {
        Some(
            match child_deadline.and_then(super::subagent_content::import_child_capture_deadline) {
                Some(child_deadline) => {
                    capture_imported_subagent_content(
                        conn,
                        &import_scope,
                        storage_root,
                        &request,
                        &subagent_discovery,
                        child_deadline,
                    )
                    .await
                }
                None => Err(anyhow!(
                    "historical child capture has no remaining work window; retry the import"
                )),
            },
        )
    } else {
        None
    };
    let child_progress = early_child_capture.as_ref().map(|result| match result {
        Ok(summary) => summary.clone(),
        Err(error) => subagent_capture_progress(error),
    });
    partial |= early_child_capture.as_ref().is_some_and(Result::is_err);
    if let Some(summary) = child_progress.as_ref() {
        partial |= historical_child_capture_is_partial(request.agent_kind, summary);
    }
    let usage_attribution = if let (Some(summary), Some(extraction)) = (
        child_progress.as_ref(),
        codex_retention
            .as_ref()
            .and_then(CodexRetention::summary_metadata),
    ) {
        let snapshot_complete = request
            .transcript_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.completeness
                    == super::capture::snapshot::CaptureSnapshotCompleteness::Complete
                    && snapshot.has_durable_source_commitment()
                    && snapshot.source.as_ref().is_some_and(|source| {
                        matches!(source.kind,
                    super::capture::snapshot::CaptureSnapshotSourceKind::ProviderFile
                    | super::capture::snapshot::CaptureSnapshotSourceKind::TrustedExport)
                    })
            });
        summary
            .project_usage_attribution(
                conn,
                storage_root,
                super::subagent_content::SubagentUsageAttributionInput {
                    scope: &import_scope,
                    capture_session: &request.session_id,
                    current_thread: &request.provider_session_id,
                    verified_cwd: &request.working_dir,
                    agent_kind: request.agent_kind.as_db_str(),
                    as_of: "import_pass",
                    parent_extraction: &extraction,
                    parent_snapshot_complete: snapshot_complete,
                    deadline: child_deadline,
                },
            )
            .await
    } else {
        None
    };
    let child_written = child_progress
        .as_ref()
        .map_or(0, |summary| summary.checkpoints_written);
    let retention_progress = if let Some(retention) = codex_retention.as_ref() {
        append_codex_retention_supplements(
            RetentionSupplementContext {
                conn,
                storage: storage_root,
                request: &request,
                scope: &import_scope,
                lease: &lease,
                deadline,
                reports: &reports,
                reservation: &outcome,
                usage_attribution: usage_attribution.as_ref(),
            },
            retention,
            &covered,
        )
        .await
        .map_err(|error| {
            import_error_with_child_progress(error, &request, &lease, &outcome, 0, child_written)
        })?
    } else {
        RetentionSupplementProgress::default()
    };
    partial |= retention_progress.pending;
    let retained_written = retention_progress.written;
    if outcome.reserved.is_empty() {
        let capture = match early_child_capture.take() {
            Some(result) => result,
            None => {
                capture_imported_subagent_content(
                    conn,
                    &import_scope,
                    storage_root,
                    &request,
                    &subagent_discovery,
                    deadline,
                )
                .await
            }
        };
        let capture_summary = match capture {
            Ok(summary) => summary,
            Err(error) => {
                let child_progress = subagent_capture_progress(&error);
                let error = abandon_import_attempt_after_error(
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
                return Err(error.context(ImportProgressError {
                    summary: ImportSummary {
                        session_id: request.session_id.clone(),
                        agent_kind: request.agent_kind.as_db_str().to_string(),
                        turns_seen: request.turns.len(),
                        checkpoints_written: retained_written,
                        skipped_covered: outcome.skipped_covered,
                        skipped_inflight: outcome.skipped_inflight,
                        conflicted: outcome.conflicted,
                        partial: true,
                    },
                    subagent_checkpoints_written: child_progress.checkpoints_written,
                    import_identity_id: lease.identity_id.clone(),
                    import_fence_token: lease.fence_token,
                }));
            }
        };
        // Non-Codex providers retain their established late capture order.
        let child_written = child_written.max(capture_summary.checkpoints_written);
        // Projection repeats completeness checks under the capture deadline;
        // preserve a newly observed incomplete child as a partial import
        // rather than discarding its durable checkpoint after discovery.
        partial |= historical_child_capture_is_partial(request.agent_kind, &capture_summary);
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
                if retained_written == 0 && child_written == 0 {
                    "failed"
                } else {
                    "partial"
                },
                error,
                deadline,
            )
            .await;
            return Err(import_error_with_child_progress(
                error,
                &request,
                &lease,
                &outcome,
                retained_written,
                child_written,
            ));
        }
        return Ok(DetailedImportSummary::new(
            ImportSummary {
                session_id: request.session_id,
                agent_kind: request.agent_kind.as_db_str().to_string(),
                turns_seen: request.turns.len(),
                checkpoints_written: retained_written,
                skipped_covered: outcome.skipped_covered,
                skipped_inflight: outcome.skipped_inflight,
                conflicted: outcome.conflicted,
                partial,
            },
            capture_summary.checkpoints_written,
            &lease,
        ));
    }

    let progress_state = if retained_written == 0 && child_written == 0 {
        "failed"
    } else {
        "partial"
    };
    let parent_commit = match async {
        #[cfg(test)]
        import_test_failpoint("before_parent_commit")?;
        current_parent_commit(conn, monotonic_deadline).await
    }
    .await
    {
        Ok(parent) => parent,
        Err(error) => {
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                progress_state,
                error,
                deadline,
            )
            .await;
            return Err(import_error_with_child_progress(
                error,
                &request,
                &lease,
                &outcome,
                retained_written,
                child_written,
            ));
        }
    };
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        let error = abandon_import_attempt_after_error(
            conn,
            &request,
            &lease,
            None,
            progress_state,
            error,
            deadline,
        )
        .await;
        return Err(import_error_with_child_progress(
            error,
            &request,
            &lease,
            &outcome,
            retained_written,
            child_written,
        ));
    }
    let mut committed_ordinals = match match coverage_versions.as_ref() {
        Some(versions) => {
            committed_source_ordinals_for_coverage(
                conn,
                &request,
                Some(versions),
                monotonic_deadline,
            )
            .await
        }
        None => committed_source_ordinals(conn, &request, monotonic_deadline).await,
    } {
        Ok(ordinals) => ordinals,
        Err(error) => {
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                progress_state,
                error,
                deadline,
            )
            .await;
            return Err(import_error_with_child_progress(
                error,
                &request,
                &lease,
                &outcome,
                retained_written,
                child_written,
            ));
        }
    };
    let mut written = retained_written;
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
            let checkpoint_id = if let Some(retained) = codex_retention.as_ref()
                .and_then(|source| source.for_turn(&turn.logical_turn_key)) {
                codex_retention_checkpoint_id(conn, &import_scope, storage_root, &request,
                    turn, retained, deadline).await?
            } else { uuid::Uuid::new_v4().to_string() };
            let now_ms = Utc::now().timestamp_millis();
            bound_checkpoint_id = Some(checkpoint_id.clone());
            let version = match coverage_versions.as_ref() {
                Some(versions) => Some(*versions.get(&turn.logical_turn_key).context("import attempt lost its coverage version binding")?),
                None => None,
            };
            let attempt_marker = match version {
                Some(version) => bind_attempt_for_coverage(conn, &request, &lease, Some(version), claim, &checkpoint_id, now_ms, deadline).await,
                None => bind_attempt(conn, &request, &lease, claim, &checkpoint_id, now_ms, deadline).await,
            }?;
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
            let projection = match version {
                Some(version) => safe_turn_projection_for_schema(version.as_i64(), request.agent_kind.as_db_str(), turn)?,
                None => safe_turn_projection(request.agent_kind.as_db_str(), turn),
            };
            let projection_line = redacted_json_line(&projection)?;
            let transcript = if let Some(retained) = codex_retention.as_ref().and_then(|source| source.for_turn(&turn.logical_turn_key)) {
                let mut bytes = projection_line.into_inner();
                bytes.extend_from_slice(retained.bytes());
                RedactedBytes::new_unchecked(bytes)
            } else {
                projection_line
            };
            let mut metadata = serde_json::json!({
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
            "redaction_report": reports.metadata,
            "transcript_snapshot": request.transcript_snapshot.clone(),
            "import": {
                "source_kind": request.source_kind,
                "source_id": request.source_id,
                "logical_turn_key": turn.logical_turn_key,
                "ordinal": turn.ordinal,
            }
            });
            if let Some(extraction) = codex_retention.as_ref().and_then(CodexRetention::summary_metadata) {
                metadata["extraction"] = extraction;
                if let Some(attribution) = usage_attribution.as_ref() {
                    metadata["usage_attribution"] = attribution.clone();
                }
            }
            let metadata = redacted_json(&metadata)?;
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
            let report = reports.full_for_turn(&turn.logical_turn_key)?;
            let source_snapshot = request
                .transcript_snapshot
                .clone()
                .context("historical import has no complete authorized source snapshot")?;
            let turn_native = per_turn_native.remove(&turn.logical_turn_key);
            if let Some(reasoning) = reasoning_identity_plan.as_ref() {
                let expected_native = reasoning
                    .turn(&turn.logical_turn_key)
                    .context("reserved import turn has no authenticated reasoning identity")?
                    .kind()
                    == crate::internal::metadata::ReasoningIdentityKind::Nonempty;
                if expected_native != turn_native.is_some() {
                    bail!("reserved import turn has an inconsistent native reasoning batch");
                }
            }
            let checkpoint_payload = match turn_native {
                Some(native) => CheckpointRedactedPayload::from_derived_turn_projection_with_reasoning(
                    transcript,
                    source_snapshot,
                    metadata,
                    lifecycle,
                    report,
                    Some(native),
                ),
                None => CheckpointRedactedPayload::from_derived_turn_projection(
                    transcript,
                    source_snapshot,
                    metadata,
                    lifecycle,
                    report,
                ),
            }
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
            let legacy_commit_plan;
            let versioned_commit_plan;
            let checkpoint_request = if let Some(version) = version {
                let bound = versioned_reserved.iter().find(|bound| bound.coverage_schema_version == version && &bound.claim == *claim)
                    .context("import commit lost its exact versioned reservation")?;
                versioned_commit_plan = VersionedLiveClaimCommitPlan::new(plan, std::slice::from_ref(bound))?;
                ImportedCheckpointWriteRequest::new_versioned(
                    &checkpoint_id, &request.session_id, request.agent_kind.as_db_str(), parent_commit.as_deref(),
                    &checkpoint_payload, &versioned_commit_plan, deadline,
                )
            } else {
                legacy_commit_plan = LiveCaptureClaimCommitPlan::new(plan, reasoning_identity_plan.as_ref())
                    .context("prepare imported reasoning identity transaction companion")?;
                ImportedCheckpointWriteRequest::new(
                    &checkpoint_id, &request.session_id, request.agent_kind.as_db_str(), parent_commit.as_deref(),
                    &checkpoint_payload, &legacy_commit_plan, deadline,
                )
            }.map_err(|error| anyhow!("prepare imported checkpoint write request: {error}"))?;
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
                    // The checkpoint row is already durable. A busy marker
                    // clear must not leave that in-flight row behind, or a
                    // retry that otherwise committed every turn still fails
                    // its recovery check.
                    if let Err(error) = ImportedCheckpointWriter::clear_committed_import_marker(
                        conn,
                        &request.session_id,
                        &checkpoint_id,
                    )
                    .await
                    {
                        tracing::warn!(
                            reason = "import_checkpoint_marker_clear_failed",
                            error = %error,
                            "committed import checkpoint kept its in-flight marker"
                        );
                    }
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
            let terminal_state = if written == 0 && child_written == 0 {
                "failed"
            } else {
                "partial"
            };
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
                return Err(import_error_with_child_progress(
                    error,
                    &request,
                    &lease,
                    &outcome,
                    written,
                    child_written,
                ));
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
            return Err(error.context(ImportProgressError {
                summary,
                subagent_checkpoints_written: child_written,
                import_identity_id: lease.identity_id.clone(),
                import_fence_token: lease.fence_token,
            }));
        }
    }
    let capture = match early_child_capture.take() {
        Some(result) => result,
        None => {
            capture_imported_subagent_content(
                conn,
                &import_scope,
                storage_root,
                &request,
                &subagent_discovery,
                deadline,
            )
            .await
        }
    };
    let capture_summary = match capture {
        Ok(summary) => summary,
        Err(error) => {
            let child_progress = subagent_capture_progress(&error);
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                "partial",
                error.context("capture imported subagent content"),
                deadline,
            )
            .await;
            return Err(error.context(ImportProgressError {
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
            }));
        }
    };
    // Projection repeats completeness checks under the capture deadline;
    // preserve a newly observed incomplete child as a partial import rather
    // than abandoning already durable parent and child checkpoints.
    partial |= historical_child_capture_is_partial(request.agent_kind, &capture_summary);
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
        let error = abandon_import_attempt_after_error(
            conn, &request, &lease, None, "partial", error, deadline,
        )
        .await;
        return Err(error.context(ImportProgressError {
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
        }));
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
pub(crate) mod tests {
    use super::*;

    async fn cov2_import_test_setup()
    -> (tempfile::TempDir, DatabaseConnection, PathBuf, CaptureScope) {
        let tmp = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let root = tmp.path().canonicalize().unwrap();
        let storage = root.join(util::ROOT_DIR);
        let conn = sea_orm::Database::connect(format!(
            "sqlite://{}",
            storage.join(util::DATABASE).display()
        ))
        .await
        .unwrap();
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        (tmp, conn, root, scope)
    }

    fn cov2_import_test_request(
        root: &Path,
        scope: &CaptureScope,
        turns: Vec<NormalizedTurn>,
    ) -> ImportRequest {
        use crate::internal::ai::capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotSource, CaptureSnapshotSourceKind,
        };
        let commitment = format!("source/hmac-v2/{}", "a".repeat(64));
        ImportRequest {
            agent_kind: AgentKind::Codex,
            provider_session_id: "cov2-direct-fixed".to_owned(),
            session_id: "codex__cov2-direct-fixed".to_owned(),
            source_kind: "file".to_owned(),
            source_id: commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "untrusted caller digest must be replaced".to_owned(),
            started_at: 1,
            ended_at: 2,
            session_state: "stopped".to_owned(),
            stopped_at: Some(2),
            working_dir: root.to_path_buf(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_owned(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: commitment.clone(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"pipeline":"typed_allowlist","raw_persisted":false,"matches":[],"bytes_scanned":16,"bytes_redacted":0}),
            transcript_snapshot: Some(CaptureSnapshotProjection {
                completeness: CaptureSnapshotCompleteness::Complete,
                partial_reason: None,
                source: Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: "not_retained:v1".to_owned(),
                    digest_sha256: Some(commitment),
                    byte_len: Some(16),
                }),
                transcript_redacted_bytes: 16,
                redaction_match_count: 0,
                redaction_bytes_scanned: 16,
                redaction_bytes_redacted: 0,
            }),
            turn_boundaries: turns
                .iter()
                .map(|turn| {
                    (
                        turn.logical_turn_key.clone(),
                        TurnBoundary {
                            started_at: 1,
                            ended_at: 2,
                        },
                    )
                })
                .collect(),
            turns,
        }
    }

    fn cov2_test_turn(key: &str, ordinal: usize) -> NormalizedTurn {
        NormalizedTurn {
            logical_turn_key: key.to_owned(),
            ordinal,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: vec![super::super::observed_agents::SemanticRecord::User {
                text: "fixed synthetic prompt".to_owned(),
            }],
        }
    }

    fn cov2_object_files(storage: &Path) -> BTreeMap<PathBuf, String> {
        let mut files = BTreeMap::new();
        let mut directories = vec![storage.join("objects")];
        while let Some(directory) = directories.pop() {
            if !directory.exists() {
                continue;
            }
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    directories.push(entry.path());
                } else {
                    files.insert(
                        entry.path(),
                        hex::encode(Sha256::digest(std::fs::read(entry.path()).unwrap())),
                    );
                }
            }
        }
        files
    }

    async fn cov2_traces_head(conn: &DatabaseConnection) -> Option<String> {
        conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT `commit` AS oid FROM reference WHERE name='traces' AND remote IS NULL AND kind='Branch'".to_owned()))
            .await.unwrap().and_then(|row| row.try_get_by::<Option<String>,_>("oid").unwrap())
    }

    async fn cov2_count(conn: &DatabaseConnection, query: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            query.to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("n")
        .unwrap()
    }

    #[cfg(unix)]
    async fn cov2_runtime_store_setup()
    -> (tempfile::TempDir, DatabaseConnection, PathBuf, CaptureScope) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let storage = root.join(util::ROOT_DIR);
        std::fs::create_dir_all(storage.join("objects")).unwrap();
        let conn =
            crate::internal::db::create_database(storage.join(util::DATABASE).to_str().unwrap())
                .await
                .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        let scope = CaptureScope::resolve(&conn, &root).await.unwrap();
        (directory, conn, root, scope)
    }

    #[cfg(unix)]
    fn cov2_runtime_source(root: &Path, include_idle: bool) -> Vec<u8> {
        let mut source = serde_json::json!({
            "info":{"id":"pure-partial-store","directory":root},
            "messages":[{"id":"u","type":"user","text":"hi"},
                {"id":"a-u","type":"assistant","content":[
                    {"id":"r-u","type":"reasoning","text":"before sk-ant-EXAMPLE0000000000000000 after"},
                    {"id":"t-u","type":"text","text":"hello"}]},
                {"id":"i-u","type":"idle","outcome":"succeeded"}]
        });
        if !include_idle {
            source["messages"].as_array_mut().unwrap().pop();
        }
        serde_json::to_vec(&source).unwrap()
    }

    #[cfg(unix)]
    async fn cov2_runtime_request(
        conn: &DatabaseConnection,
        root: &Path,
        scope: &CaptureScope,
        include_idle: bool,
    ) -> (ImportRequest, CaptureCoverageRuntime) {
        cov2_runtime_request_from_source(conn, root, scope, cov2_runtime_source(root, include_idle))
            .await
    }

    #[cfg(unix)]
    async fn cov2_runtime_request_from_source(
        conn: &DatabaseConnection,
        root: &Path,
        scope: &CaptureScope,
        source: Vec<u8>,
    ) -> (ImportRequest, CaptureCoverageRuntime) {
        let (projection, native) = prepare_import_projection_with_reasoning(
            AgentKind::OpenCode,
            import_provider_commitment("pure-partial-store"),
            source,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert!(native.is_none());
        let bytes = serde_json::to_vec(&projection).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sk-ant-EXAMPLE"));
        let projection: PreparedImportProjection = serde_json::from_slice(&bytes).unwrap();
        cov2_runtime_request_from_projection(conn, root, scope, projection).await
    }

    #[cfg(unix)]
    async fn cov2_runtime_request_from_projection(
        conn: &DatabaseConnection,
        root: &Path,
        scope: &CaptureScope,
        mut projection: PreparedImportProjection,
    ) -> (ImportRequest, CaptureCoverageRuntime) {
        use crate::internal::ai::observed_agents::coverage::decode_redacted_import_runtime;
        let (turns, classifier_rejected, source_partial) = decode_redacted_import_runtime(
            projection.coverage_runtime.take().unwrap(),
            "opencode",
            &projection.turns,
            &projection.session_state,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        let storage = root.join(util::ROOT_DIR);
        let source_preimage = import_source_preimage(
            AgentKind::OpenCode,
            "export",
            "pure-partial-store",
            "pure-partial-store",
        )
        .unwrap();
        let commitment =
            super::super::capture::key::derive_capture_source_commitment_in_scope_until(
                conn,
                scope,
                &storage,
                root,
                super::super::capture::key::CaptureSourceCommitmentDomain::ImportSourceV2,
                &source_preimage,
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap();
        let snapshot_preimage = projection
            .transcript_snapshot
            .as_ref()
            .unwrap()
            .redacted_digest_preimage()
            .unwrap();
        let snapshot_commitment =
            super::super::capture::key::derive_snapshot_content_commitment_in_scope_until(
                conn,
                scope,
                &storage,
                root,
                &snapshot_preimage,
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert_ne!(
            commitment, snapshot_commitment,
            "source and snapshot commitments use separate domains"
        );
        assert!(
            projection
                .transcript_snapshot
                .as_mut()
                .unwrap()
                .bind_source_commitment(snapshot_commitment)
        );
        let mut request = import_request_from_projection(
            AgentKind::OpenCode,
            "pure-partial-store".to_owned(),
            "export".to_owned(),
            commitment,
            root.to_path_buf(),
            projection,
        );
        request.capture_scope = Some(scope.clone());
        let existing = load_existing_session_ownership_in_scope(
            conn,
            AgentKind::OpenCode,
            "pure-partial-store",
            scope,
        )
        .await
        .unwrap();
        validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
        (
            request,
            CaptureCoverageRuntime {
                turns,
                classifier_rejected,
                source_partial,
            },
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_import_16_mib_ordinary_runtime_frame_fits_existing_cap() {
        let (_tmp, _conn, root, _scope) = cov2_runtime_store_setup().await;
        let mut document = serde_json::json!({"info":{"id":"large-ordinary","directory":root},
            "messages":[{"id":"u","type":"user","text":""}]});
        let overhead = serde_json::to_vec(&document).unwrap().len();
        document["messages"][0]["text"] = "x"
            .repeat(usize::try_from(TRANSCRIPT_READ_HARD_CAP_BYTES).unwrap() - overhead)
            .into();
        let bytes = serde_json::to_vec(&document).unwrap();
        assert_eq!(bytes.len() as u64, TRANSCRIPT_READ_HARD_CAP_BYTES);
        let (projection, native) = prepare_import_projection_with_reasoning(
            AgentKind::OpenCode,
            import_provider_commitment("large-ordinary"),
            bytes,
            Instant::now() + Duration::from_secs(120),
        )
        .unwrap();
        assert!(native.is_none());
        assert!(
            !super::super::observed_agents::coverage::import_runtime_has_typed_turns(
                projection.coverage_runtime.as_ref().unwrap()
            )
        );
        let frame = super::super::observed_agents::coverage::encode_bounded_json(
            &projection,
            usize::try_from(crate::command::agent::IMPORT_PREPARATION_HELPER_OUTPUT_CAP).unwrap(),
        )
        .unwrap();
        assert!(frame.len() as u64 <= crate::command::agent::IMPORT_PREPARATION_HELPER_OUTPUT_CAP);
        assert_eq!(source_digest(&projection.turns), projection.content_digest);
    }

    /// Original VER2: actual snapshot/helper projection and private import
    /// route, real SQLite/object store, independent redacted live/import gold.
    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_direct_visible_import_and_v1_parity() {
        const V2: &[u8] = br#"[{"role":"user","text":"hi"},{"provider":"opencode","role":"reasoning","source_kind":"reasoning","text":"before <REDACTED:anthropic-api-key> after"},{"role":"assistant","text":"hello"}]"#;
        const V2_SHA: &str = "7973604f43b515e1af6b1206e85c4fdc88447eed5a7822ca0ec3a77224b792a9";
        const V1: &[u8] = br#"[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"}]"#;
        const V1_SHA: &str = "df991cd9a1ac5c12c32b8cdf0254c3dfbbf26485b505a5afc83a90d1128ebc54";
        let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let (legacy, _) = cov2_runtime_request(&conn, &root, &scope, true).await;
        assert_eq!(
            super::super::observed_agents::canonical_turn_bytes(&legacy.turns[0].records),
            V1
        );
        assert_eq!(legacy.turns[0].digest_hex(), V1_SHA);
        let old_source_digest = source_digest(&legacy.turns);
        assert_eq!(
            source_digest_for_coverage_versions(&legacy.turns, &[1]).unwrap(),
            old_source_digest
        );
        let first = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            legacy,
            &[1],
            Duration::from_secs(30),
            Some(&native_import_test_helper_program()),
        )
        .await
        .unwrap();
        assert_eq!(first.checkpoints_written, 1);
        let (request, runtime) = cov2_runtime_request(&conn, &root, &scope, true).await;
        let turn = &runtime.turns.as_ref().unwrap()[0];
        assert_eq!(turn.logical_turn_key, "u");
        assert_eq!(turn.ordinal, 0);
        assert_eq!(turn.completeness, Completeness::Incomplete);
        assert_eq!(
            request.session_state, "active",
            "turn idle is not session termination"
        );
        let encoded = encode_coverage_turn_for_schema(2, turn).unwrap();
        assert_eq!(encoded.canonical_bytes, V2);
        assert_eq!(encoded.digest_hex, V2_SHA);
        let expected_source_digest =
            source_digest_for_coverage_versions(runtime.turns.as_ref().unwrap(), &[2]).unwrap();
        assert_ne!(expected_source_digest, old_source_digest);
        let captured = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(captured.summary.checkpoints_written, 1);
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2 AND completeness='incomplete' AND state='catalog_committed'").await, 1);
        // Read the committed transcript object, not just the pre-write codec.
        use crate::internal::ai::checkpoint_reader as reader;
        let stored = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT c.coverage_digest, c.checkpoint_id, p.tree_oid FROM agent_coverage_claim c
             JOIN agent_checkpoint p ON p.checkpoint_id=c.checkpoint_id WHERE c.coverage_schema_version=2".to_owned()))
            .await.unwrap().unwrap();
        assert_eq!(
            stored.try_get_by::<String, _>("coverage_digest").unwrap(),
            V2_SHA
        );
        let checkpoint_id = stored.try_get_by::<String, _>("checkpoint_id").unwrap();
        let tree = reader::read_tree_object(
            &storage,
            &stored.try_get_by::<String, _>("tree_oid").unwrap(),
        )
        .unwrap();
        let wrapped = reader::subtree(&storage, &tree, "checkpoint").unwrap();
        let prefix = reader::subtree(&storage, &wrapped, &checkpoint_id[..2]).unwrap();
        let leaf = reader::subtree(&storage, &prefix, &checkpoint_id[2..]).unwrap();
        let transcript = reader::subtree(&storage, &leaf, "transcript").unwrap();
        let transcript_oid = &reader::tree_entry(&transcript, "opencode.jsonl")
            .unwrap()
            .id;
        let (bytes, truncated) =
            reader::read_checkpoint_object_bounded(&storage, transcript_oid, 4096).unwrap();
        assert!(!truncated);
        let persisted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted["schema_version"], 2);
        assert_eq!(persisted["completeness"], "incomplete");
        assert_eq!(persisted["records"][1]["type"], "reasoning");
        assert_eq!(
            persisted["records"][1]["text"],
            "before <REDACTED:anthropic-api-key> after"
        );
        let row = conn.query_one_raw(Statement::from_sql_and_values(conn.get_database_backend(),
            "SELECT coverage_digest FROM agent_coverage_claim WHERE coverage_schema_version=1 AND logical_turn_key='u'",
            [])).await.unwrap().unwrap();
        assert_eq!(
            row.try_get_by::<String, _>("coverage_digest").unwrap(),
            V1_SHA
        );
        let identity = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT observed_digest FROM agent_import_identity".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            identity.try_get_by::<String, _>("observed_digest").unwrap(),
            expected_source_digest
        );
        let (request, runtime) = cov2_runtime_request(&conn, &root, &scope, true).await;
        let repeated = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(repeated.summary.checkpoints_written, 0);
        assert_eq!(repeated.summary.skipped_covered, 1);
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            2
        );
        let before = cov2_object_files(&storage);
        let before_ref = cov2_traces_head(&conn).await;
        // The actual command preflight must stop a rejected delivery before
        // migration/index leases; new ordinary keys cannot evade v2 history.
        let (mut rejected_request, _) = cov2_runtime_request(&conn, &root, &scope, true).await;
        let mut unrelated = rejected_request.turns[0].clone();
        unrelated.logical_turn_key = "unrelated-new-ordinary".to_owned();
        unrelated.ordinal = 1;
        rejected_request.turns = vec![unrelated];
        rejected_request.turn_boundaries = BTreeMap::from([(
            "unrelated-new-ordinary".to_owned(),
            TurnBoundary {
                started_at: rejected_request.started_at,
                ended_at: rejected_request.ended_at,
            },
        )]);
        let error = async {
            coverage_gate::ensure_runtime_import_source_history_until(
                &conn,
                &scope,
                &rejected_request.session_id,
                commit_deadline_after(Duration::from_secs(10)),
                true,
            )
            .await?;
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                rejected_request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(CaptureCoverageRuntime {
                    turns: None,
                    classifier_rejected: true,
                    source_partial: false,
                }),
            )
            .await
        }
        .await
        .unwrap_err();
        assert!(error.to_string().contains("start a new session"));
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=1"
            )
            .await,
            1
        );
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            2
        );
        assert_eq!(cov2_object_files(&storage), before);
        assert_eq!(cov2_traces_head(&conn).await, before_ref);
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_import_identity WHERE next_ordinal=1 AND state='committed'").await, 1);

        let (_active_tmp, active_conn, active_root, active_scope) =
            cov2_runtime_store_setup().await;
        let (request, runtime) =
            cov2_runtime_request(&active_conn, &active_root, &active_scope, false).await;
        assert_eq!(request.session_state, "active");
        assert_eq!(
            runtime.turns.as_ref().unwrap()[0].completeness,
            Completeness::Incomplete
        );
        let active_storage = active_root.join(util::ROOT_DIR);
        let active = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &active_conn,
                &active_storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(active.summary.checkpoints_written, 1);
        assert_eq!(cov2_count(&active_conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2 AND completeness='complete'").await, 0);
        assert_eq!(cov2_count(&active_conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2 AND completeness='incomplete'").await, 1);

        let (_mixed_tmp, mixed_conn, mixed_root, mixed_scope) = cov2_runtime_store_setup().await;
        let mixed_source = serde_json::to_vec(&serde_json::json!({"info":{"id":"pure-partial-store","directory":mixed_root},
            "messages":[{"id":"z","type":"user","text":"first"},
              {"id":"az","type":"assistant","content":[{"id":"rz","type":"reasoning","text":"visible"},{"id":"tz","type":"text","text":"answer"}]},
              {"id":"iz","type":"idle","outcome":"succeeded"},
              {"id":"a","type":"user","text":"second"},
              {"id":"aa","type":"assistant","content":[{"id":"ta","type":"text","text":"plain"}]},
              {"id":"ia","type":"idle","outcome":"succeeded"}]})).unwrap();
        let (request, runtime) =
            cov2_runtime_request_from_source(&mixed_conn, &mixed_root, &mixed_scope, mixed_source)
                .await;
        assert_eq!(
            runtime
                .turns
                .as_ref()
                .unwrap()
                .iter()
                .map(select_coverage_schema)
                .collect::<Vec<_>>(),
            [CoverageSchemaVersion::V2, CoverageSchemaVersion::V1]
        );
        let mixed_storage = mixed_root.join(util::ROOT_DIR);
        let mixed = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &mixed_conn,
                &mixed_storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(mixed.summary.checkpoints_written, 2);
        assert_eq!(cov2_count(&mixed_conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key='z' AND coverage_schema_version=2 AND state='catalog_committed'").await,1);
        assert_eq!(cov2_count(&mixed_conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key='a' AND coverage_schema_version=1 AND state='catalog_committed'").await,1);
        assert_eq!(cov2_count(&mixed_conn,"SELECT COUNT(*) AS n FROM agent_import_identity WHERE next_ordinal=2 AND state='committed'").await,1);

        let (_fresh_tmp, fresh_conn, fresh_root, fresh_scope) = cov2_runtime_store_setup().await;
        let (request, _) = cov2_runtime_request(&fresh_conn, &fresh_root, &fresh_scope, true).await;
        coverage_gate::ensure_runtime_import_source_history_until(
            &fresh_conn,
            &fresh_scope,
            &request.session_id,
            commit_deadline_after(Duration::from_secs(10)),
            true,
        )
        .await
        .unwrap();
        let fresh = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &fresh_conn,
                &fresh_root.join(util::ROOT_DIR),
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(CaptureCoverageRuntime {
                    turns: None,
                    classifier_rejected: true,
                    source_partial: false,
                }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(fresh.summary.checkpoints_written, 1);
        assert_eq!(
            cov2_count(
                &fresh_conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2"
            )
            .await,
            0
        );
        let row = fresh_conn
            .query_one_raw(Statement::from_string(
                fresh_conn.get_database_backend(),
                "SELECT coverage_digest FROM agent_coverage_claim".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get_by::<String, _>("coverage_digest").unwrap(),
            V1_SHA
        );
    }

    #[cfg(unix)]
    fn cov2_partial_classifier_ordinary_sources(root: &Path) -> Vec<(&'static str, Vec<u8>)> {
        let mut base: serde_json::Value =
            serde_json::from_slice(&cov2_runtime_source(root, true)).unwrap();
        base["messages"][1]["content"][0]["text"] = "readable".into();
        let mut sources = Vec::new();
        for (label, text) in [("empty-no-state", ""), ("whitespace-no-state", " \t\n ")] {
            let mut source = base.clone();
            source["messages"][1]["content"][0]["text"] = text.into();
            sources.push((label, serde_json::to_vec(&source).unwrap()));
        }
        for (label, field) in [
            ("missing-part-id", "part"),
            ("missing-assistant-id", "message"),
        ] {
            let mut source = base.clone();
            let target = if field == "part" {
                &mut source["messages"][1]["content"][0]
            } else {
                &mut source["messages"][1]
            };
            target.as_object_mut().unwrap().remove("id");
            sources.push((label, serde_json::to_vec(&source).unwrap()));
        }
        for (label, field) in [
            ("foreign-session-coordinate", "sessionID"),
            ("foreign-message-coordinate", "messageID"),
        ] {
            let mut source = base.clone();
            source["messages"][1]["content"][0][field] = "foreign".into();
            sources.push((label, serde_json::to_vec(&source).unwrap()));
        }
        // Native tool call ids are allowed to repeat by the old scrubber.
        // The typed classifier sees this shared part coordinate as ambiguous.
        // Duplicate reasoning/message ids themselves remain old refusals.
        base["messages"][1]["content"].as_array_mut().unwrap().push(
            serde_json::json!({"id":"r-u","type":"tool","name":"read",
                "state":{"status":"completed","input":{},
                    "content":[{"type":"text","text":"ok"}]}}),
        );
        sources.push((
            "duplicate-tool-part-coordinate",
            serde_json::to_vec(&base).unwrap(),
        ));
        sources
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_partial_classifier_ordinary_import_preserves_v1() {
        use super::super::observed_agents::{canonical_turn_bytes, opencode_reasoning};
        const V1: &[u8] = br#"[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"}]"#;
        const V1_WITH_TOOL: &[u8] = br#"[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"},{"call_id":"r-u","input":{},"name":"read","role":"tool_call"},{"call_id":"r-u","content":"ok","is_error":false,"role":"tool_result"}]"#;
        for index in 0..7 {
            let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
            let (label, source) =
                cov2_partial_classifier_ordinary_sources(&root).swap_remove(index);
            // Read-only legacy oracle: no source mapping or native witness is
            // granted here. The actual import below gets its own scoped HMACs.
            let prepared = opencode_reasoning::prepare_authorized_source(source.clone()).unwrap();
            let (ordinary, disposition) =
                ReasoningSourceDisposition::from_prepared_authorized_source(
                    prepared,
                    false,
                    TRANSCRIPT_READ_HARD_CAP_BYTES,
                    "opencode",
                    "unused-legacy-oracle",
                    None,
                )
                .unwrap();
            assert!(disposition.is_ordinary(), "old scrubber must admit {label}");
            let mut legacy = normalize_opencode_export_until(
                &ordinary,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
            redact_turns_with_report(&mut legacy);
            assert_eq!(legacy.len(), 1, "{label}");
            legacy[0].completeness = Completeness::Incomplete;
            let canonical = if label == "duplicate-tool-part-coordinate" {
                V1_WITH_TOOL
            } else {
                V1
            };
            assert_eq!(
                canonical_turn_bytes(&legacy[0].records),
                canonical,
                "{label}"
            );
            let expected_digest = source_digest(&legacy);
            let (request, runtime) =
                cov2_runtime_request_from_source(&conn, &root, &scope, source.clone()).await;
            assert!(runtime.classifier_rejected, "{label}");
            assert!(!runtime.source_partial, "{label}");
            assert!(runtime.turns.is_none(), "{label}");
            assert_eq!(request.session_state, "active", "{label}");
            assert_eq!(
                request.turns[0].completeness,
                Completeness::Incomplete,
                "{label}"
            );
            assert_eq!(
                canonical_turn_bytes(&request.turns[0].records),
                canonical,
                "{label}"
            );
            assert_eq!(request.content_digest, expected_digest, "{label}");
            coverage_gate::ensure_runtime_import_source_history_until(
                &conn,
                &scope,
                &request.session_id,
                commit_deadline_after(Duration::from_secs(30)),
                runtime.classifier_rejected,
            )
            .await
            .unwrap();
            let storage = root.join(util::ROOT_DIR);
            if label.starts_with("foreign-") {
                // 33a5's strict preparer drops these optional coordinates,
                // but its snapshot keeps raw source byte_len and reports the
                // shorter ordinary scan. Its catalog already refuses this
                // mismatch. Ordinary disposition alone never admitted it.
                let projection = request.transcript_snapshot.as_ref().unwrap();
                let (_, legacy_report) = Redactor::new_default().redact(&ordinary);
                assert!(source.len() > ordinary.len(), "{label}");
                assert_eq!(
                    projection.source.as_ref().unwrap().byte_len,
                    Some(source.len() as u64),
                    "{label}"
                );
                assert_eq!(
                    projection.redaction_bytes_scanned,
                    ordinary.len(),
                    "{label}"
                );
                assert_eq!(
                    projection.redaction_bytes_scanned, legacy_report.bytes_scanned,
                    "{label}"
                );
                eprintln!(
                    "{label}: raw={} ordinary={} scan={} output={}",
                    source.len(),
                    ordinary.len(),
                    projection.redaction_bytes_scanned,
                    projection.transcript_redacted_bytes
                );
                let before_objects = cov2_object_files(&storage);
                let before_ref = cov2_traces_head(&conn).await;
                let error = import_prepared_with_coverage_versions(
                    &conn,
                    &storage,
                    request,
                    &[1],
                    Duration::from_secs(30),
                    Some(&native_import_test_helper_program()),
                )
                .await
                .unwrap_err();
                assert!(format!("{error:#}").contains("prepare historical import source ownership: capture catalog request is invalid"), "legacy {label}: {error:#}");
                let (request, runtime) =
                    cov2_runtime_request_from_source(&conn, &root, &scope, source).await;
                let error = super::super::authorized_read::with_test_helper_program(
                    native_import_test_helper_program(),
                    import_prepared_with_subagent_discovery_and_runtime(
                        &conn,
                        &storage,
                        request,
                        commit_deadline_after(Duration::from_secs(30)),
                        super::super::subagent_content::SubagentDiscovery::default(),
                        None,
                        Some(runtime),
                    ),
                )
                .await
                .unwrap_err();
                assert!(format!("{error:#}").contains("prepare historical import source ownership: capture catalog request is invalid"), "runtime {label}: {error:#}");
                assert_eq!(
                    cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
                    0,
                    "{label}"
                );
                assert_eq!(
                    cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                    0,
                    "{label}"
                );
                assert_eq!(
                    cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_import_identity").await,
                    0,
                    "{label}"
                );
                assert_eq!(cov2_object_files(&storage), before_objects, "{label}");
                assert_eq!(cov2_traces_head(&conn).await, before_ref, "{label}");
                continue;
            }
            let imported = super::super::authorized_read::with_test_helper_program(
                native_import_test_helper_program(),
                import_prepared_with_subagent_discovery_and_runtime(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    None,
                    Some(runtime),
                ),
            )
            .await
            .unwrap_or_else(|error| panic!("{label}: {error:#}"));
            assert_eq!(imported.summary.checkpoints_written, 1, "{label}");
            assert!(!imported.summary.partial, "{label}");
            assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=1 AND completeness='incomplete' AND state='catalog_committed'").await, 1, "{label}");
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2"
                )
                .await,
                0,
                "{label}"
            );
            let row = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
                "SELECT observed_digest FROM agent_import_identity WHERE next_ordinal=1 AND state='committed'".to_owned()))
                .await.unwrap().unwrap();
            assert_eq!(
                row.try_get_by::<String, _>("observed_digest").unwrap(),
                expected_digest,
                "{label}"
            );
            let before_objects = cov2_object_files(&storage);
            let before_ref = cov2_traces_head(&conn).await;
            let (request, runtime) =
                cov2_runtime_request_from_source(&conn, &root, &scope, source).await;
            let repeated = super::super::authorized_read::with_test_helper_program(
                native_import_test_helper_program(),
                import_prepared_with_subagent_discovery_and_runtime(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    None,
                    Some(runtime),
                ),
            )
            .await
            .unwrap();
            assert_eq!(repeated.summary.checkpoints_written, 0, "{label}");
            assert_eq!(repeated.summary.skipped_covered, 1, "{label}");
            assert!(!repeated.summary.partial, "{label}");
            assert_eq!(
                cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1,
                "{label}"
            );
            assert_eq!(cov2_object_files(&storage), before_objects, "{label}");
            assert_eq!(cov2_traces_head(&conn).await, before_ref, "{label}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_partial_classifier_ordinary_import_rejects_v2_history() {
        let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let (request, runtime) = cov2_runtime_request(&conn, &root, &scope, true).await;
        let imported = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(imported.summary.checkpoints_written, 1);
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2 AND state='catalog_committed'").await, 1);
        let before_objects = cov2_object_files(&storage);
        let before_ref = cov2_traces_head(&conn).await;
        for (label, source) in cov2_partial_classifier_ordinary_sources(&root) {
            let mut source: serde_json::Value = serde_json::from_slice(&source).unwrap();
            // Neither delivered key has a v2 row. Only the same-session
            // history barrier can reject this entire unrelated delivery.
            source["messages"][0]["id"] = "unrelated-new-ordinary".into();
            source["messages"].as_array_mut().unwrap().extend([
                serde_json::json!({"id":"another-new-ordinary","type":"user","text":"new"}),
                serde_json::json!({"id":"a-new","type":"assistant","content":[
                    {"id":"t-new","type":"text","text":"plain"}]}),
                serde_json::json!({"id":"i-new","type":"idle","outcome":"succeeded"}),
            ]);
            let (request, runtime) = cov2_runtime_request_from_source(
                &conn,
                &root,
                &scope,
                serde_json::to_vec(&source).unwrap(),
            )
            .await;
            assert!(runtime.classifier_rejected, "{label}");
            assert!(!runtime.source_partial, "{label}");
            assert!(runtime.turns.is_none(), "{label}");
            assert_eq!(request.turns.len(), 2, "{label}");
            assert_eq!(
                request.turns[0].logical_turn_key, "unrelated-new-ordinary",
                "{label}"
            );
            assert_eq!(
                request.turns[1].logical_turn_key, "another-new-ordinary",
                "{label}"
            );
            assert_eq!(
                request.turns[1].completeness,
                Completeness::Incomplete,
                "{label}"
            );
            let error = coverage_gate::ensure_runtime_import_source_history_until(
                &conn,
                &scope,
                &request.session_id,
                commit_deadline_after(Duration::from_secs(30)),
                runtime.classifier_rejected,
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("start a new session"),
                "{label}: {error}"
            );
            // Also drive the transaction recheck independently of preflight:
            // the new ordinary key cannot reserve behind existing v2 history.
            let error = super::super::authorized_read::with_test_helper_program(
                native_import_test_helper_program(),
                import_prepared_with_subagent_discovery_and_runtime(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    None,
                    Some(runtime),
                ),
            )
            .await
            .unwrap_err();
            if label.starts_with("foreign-") {
                assert!(format!("{error:#}").contains("prepare historical import source ownership: capture catalog request is invalid"), "legacy catalog refusal {label}: {error:#}");
            } else {
                assert!(
                    format!("{error:#}").contains("start a new session"),
                    "{label}: {error:#}"
                );
            }
            assert_eq!(
                cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
                1,
                "{label}"
            );
            assert_eq!(
                cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1,
                "{label}"
            );
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_import_identity WHERE next_ordinal<>1"
                )
                .await,
                0,
                "{label}"
            );
            assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key='unrelated-new-ordinary'").await, 0, "{label}");
            assert_eq!(cov2_object_files(&storage), before_objects, "{label}");
            assert_eq!(cov2_traces_head(&conn).await, before_ref, "{label}");
        }
    }

    #[cfg(unix)]
    async fn cov2_descriptor_projection_from_source(source: &[u8]) -> PreparedImportProjection {
        use std::io::{Seek, SeekFrom, Write};

        use crate::command::agent::{
            IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV, IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG,
            IMPORT_PREPARATION_HELPER_OUTPUT_CAP,
        };
        let mut held = tempfile::tempfile().unwrap();
        held.write_all(source).unwrap();
        held.seek(SeekFrom::Start(0)).unwrap();
        let control = serde_json::json!({"agent_kind":"opencode","source_kind":"export",
            "provider_commitment":import_provider_commitment("pure-partial-store"),
            "read_cap":TRANSCRIPT_READ_HARD_CAP_BYTES,"remaining_ms":30_000});
        let mut command = tokio::process::Command::new(native_import_test_helper_program());
        command
            .arg(IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG)
            .env_clear()
            .current_dir("/")
            .env(
                IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV,
                serde_json::to_string(&control).unwrap(),
            )
            .stdin(Stdio::from(held))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        let mut child = CancellationSafeChild::new_process_group(command.spawn().unwrap());
        let mut stdout = child.child_mut().unwrap().stdout.take().unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            read_async_strictly_bounded(&mut stdout, IMPORT_PREPARATION_HELPER_OUTPUT_CAP),
        )
        .await
        .unwrap()
        .unwrap();
        let status =
            tokio::time::timeout(Duration::from_secs(30), child.child_mut().unwrap().wait())
                .await
                .unwrap()
                .unwrap();
        child.disarm_child_after_wait();
        child.finish();
        assert!(status.success());
        let mut response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(
            response["status"], "ok",
            "descriptor preparation: {response}"
        );
        assert_eq!(response["raw_bytes"], source.len() as u64);
        // No transport key is sent: this is only the actual ordinary source
        // descriptor route and cannot admit any native provenance result.
        assert!(response.get("transport").is_none());
        assert!(response.get("transport_required").is_none());
        serde_json::from_value(response["projection"].take()).unwrap()
    }

    #[cfg(unix)]
    fn cov2_credential_uri_structure_source(root: &Path) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "info":{"id":"pure-partial-store","directory":root},
            "messages":[{"id":"unrelated-uri","type":"user",
                "text":"postgres://user:pass@host/db"},
              {"id":"a-uri","type":"assistant","content":[
                {"id":"r-uri","type":"reasoning","text":"visible thinking"},
                {"id":"t-uri","type":"text","text":"hello"}]},
              {"id":"i-uri","type":"idle","outcome":"succeeded"},
              {"id":"another-new-ordinary","type":"user","text":"new"},
              {"id":"a-new","type":"assistant","content":[
                {"id":"t-new","type":"text","text":"plain"}]},
              {"id":"i-new","type":"idle","outcome":"succeeded"}]
        }))
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_credential_uri_structure_import_retains_legacy_and_rejects_history() {
        use super::super::observed_agents::{canonical_turn_bytes, opencode_reasoning};
        let mut legacy_observed_digest = None;
        for runtime_mode in [false, true] {
            let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
            let storage = root.join(util::ROOT_DIR);
            let source = cov2_credential_uri_structure_source(&root);
            // The unchanged strict scrubber and raw redactor define the old
            // bytes/report; do not invent a repaired JSON or synthetic gold.
            let prepared = opencode_reasoning::prepare_authorized_source(source.clone()).unwrap();
            let (ordinary, disposition) =
                ReasoningSourceDisposition::from_prepared_authorized_source(
                    prepared,
                    false,
                    TRANSCRIPT_READ_HARD_CAP_BYTES,
                    "opencode",
                    "legacy-oracle",
                    None,
                )
                .unwrap();
            assert!(disposition.is_ordinary());
            let (first_pass, first_report) = Redactor::new_default().redact(&ordinary);
            assert!(
                first_report
                    .matches
                    .iter()
                    .any(|hit| hit.rule_id == "credential-uri")
            );
            assert!(
                serde_json::from_slice::<serde_json::Value>(first_pass.bytes()).is_err(),
                "the raw credential URI rule consumes adjacent JSON syntax"
            );
            assert!(!String::from_utf8_lossy(first_pass.bytes()).contains("user:pass@host"));
            let mut legacy_turns = normalize_opencode_export_until(
                first_pass.bytes(),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
            let typed_report = redact_turns_with_report(&mut legacy_turns);
            assert_eq!(legacy_turns.len(), 1);
            assert!(legacy_turns[0].records.is_empty());
            assert_eq!(legacy_turns[0].completeness, Completeness::Incomplete);
            let expected_content = source_digest(&legacy_turns);
            let projection = cov2_descriptor_projection_from_source(&source).await;
            let snapshot = projection.transcript_snapshot.as_ref().unwrap();
            let first_pass_sha: [u8; 32] = Sha256::digest(first_pass.bytes()).into();
            assert_eq!(snapshot.redacted_digest_preimage(), Some(first_pass_sha));
            assert_eq!(snapshot.transcript_redacted_bytes, first_pass.len());
            assert_eq!(snapshot.redaction_match_count, first_report.match_count());
            assert_eq!(snapshot.redaction_bytes_scanned, first_report.bytes_scanned);
            assert_eq!(
                snapshot.redaction_bytes_redacted,
                first_report.bytes_redacted
            );
            assert_eq!(
                projection.redaction_report,
                merged_import_redaction_report(&first_report, &typed_report)
            );
            assert_eq!(projection.content_digest, expected_content);
            assert_eq!(
                canonical_turn_bytes(&projection.turns[0].records),
                canonical_turn_bytes(&legacy_turns[0].records)
            );
            let (request, runtime) =
                cov2_runtime_request_from_projection(&conn, &root, &scope, projection).await;
            assert!(runtime.classifier_rejected);
            assert!(!runtime.source_partial);
            assert!(runtime.turns.is_none());
            assert_eq!(request.session_state, "active");
            assert_eq!(request.turns[0].completeness, Completeness::Incomplete);
            coverage_gate::ensure_runtime_import_source_history_until(
                &conn,
                &scope,
                &request.session_id,
                commit_deadline_after(Duration::from_secs(30)),
                runtime.classifier_rejected,
            )
            .await
            .unwrap();
            let summary = if runtime_mode {
                super::super::authorized_read::with_test_helper_program(
                    native_import_test_helper_program(),
                    import_prepared_with_subagent_discovery_and_runtime(
                        &conn,
                        &storage,
                        request,
                        commit_deadline_after(Duration::from_secs(30)),
                        super::super::subagent_content::SubagentDiscovery::default(),
                        None,
                        Some(runtime),
                    ),
                )
                .await
                .unwrap()
                .summary
            } else {
                import_prepared_with_coverage_versions(
                    &conn,
                    &storage,
                    request,
                    &[1],
                    Duration::from_secs(30),
                    Some(&native_import_test_helper_program()),
                )
                .await
                .unwrap()
            };
            assert_eq!(summary.checkpoints_written, 1);
            assert!(!summary.partial);
            assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=1 AND completeness='incomplete' AND state='catalog_committed'").await, 1);
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2"
                )
                .await,
                0
            );
            let row = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
                "SELECT observed_digest FROM agent_import_identity WHERE next_ordinal=1 AND state='committed'".to_owned()))
                .await.unwrap().unwrap();
            let observed = row.try_get_by::<String, _>("observed_digest").unwrap();
            assert_eq!(observed, expected_content);
            if runtime_mode {
                assert_eq!(Some(observed), legacy_observed_digest);
                let before_objects = cov2_object_files(&storage);
                let before_ref = cov2_traces_head(&conn).await;
                let projection = cov2_descriptor_projection_from_source(&source).await;
                let (request, runtime) =
                    cov2_runtime_request_from_projection(&conn, &root, &scope, projection).await;
                let replay = super::super::authorized_read::with_test_helper_program(
                    native_import_test_helper_program(),
                    import_prepared_with_subagent_discovery_and_runtime(
                        &conn,
                        &storage,
                        request,
                        commit_deadline_after(Duration::from_secs(30)),
                        super::super::subagent_content::SubagentDiscovery::default(),
                        None,
                        Some(runtime),
                    ),
                )
                .await
                .unwrap();
                assert_eq!(replay.summary.checkpoints_written, 0);
                assert_eq!(replay.summary.skipped_covered, 1);
                assert_eq!(
                    cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                    1
                );
                assert_eq!(cov2_object_files(&storage), before_objects);
                assert_eq!(cov2_traces_head(&conn).await, before_ref);
            } else {
                legacy_observed_digest = Some(observed);
            }
        }
        let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let (request, runtime) = cov2_runtime_request(&conn, &root, &scope, true).await;
        let seeded = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap();
        assert_eq!(seeded.summary.checkpoints_written, 1);
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key='u' AND coverage_schema_version=2 AND state='catalog_committed'").await, 1);
        let before_objects = cov2_object_files(&storage);
        let before_ref = cov2_traces_head(&conn).await;
        let source = cov2_credential_uri_structure_source(&root);
        let projection = cov2_descriptor_projection_from_source(&source).await;
        let (request, runtime) =
            cov2_runtime_request_from_projection(&conn, &root, &scope, projection).await;
        assert!(runtime.classifier_rejected);
        assert!(!runtime.source_partial);
        assert!(runtime.turns.is_none());
        assert_eq!(request.turns.len(), 1);
        assert!(
            request
                .turns
                .iter()
                .all(|turn| turn.logical_turn_key != "u")
        );
        assert!(request.turns[0].records.is_empty());
        let error = coverage_gate::ensure_runtime_import_source_history_until(
            &conn,
            &scope,
            &request.session_id,
            commit_deadline_after(Duration::from_secs(30)),
            runtime.classifier_rejected,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("start a new session"));
        // The recheck must precede the zero-record/no-op path as well as any
        // new ordinary key reservation, even when no delivered key has v2.
        let error = super::super::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery_and_runtime(
                &conn,
                &storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                Some(runtime),
            ),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("start a new session"));
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            1
        );
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_import_identity WHERE next_ordinal<>1"
            )
            .await,
            0
        );
        assert_eq!(cov2_object_files(&storage), before_objects);
        assert_eq!(cov2_traces_head(&conn).await, before_ref);
    }

    // Finite constants from actual dcc468b original-helper execution, not the
    // current serializer. Raw 021eaae37ae7bfd4fa734e904198496be0ad081e89dedd47c6d18805af5a8b78,
    // 2026-10-15 01:00:59..01:32:18 UTC, old exact1/1 exit0. The path-bound
    // control and three unsampled DB helpers are not portable legacy oracles.
    #[tokio::test]
    async fn cov2_import_cursor_and_digest_golden() {
        {
            let turns: Vec<NormalizedTurn> = serde_json::from_str(r###"[]"###).unwrap();
            assert_eq!(
                source_digest(&turns),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
            assert_eq!(
                source_digest_for_coverage_versions(&turns, &vec![1; turns.len()]).unwrap(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
        }
        {
            let turns: Vec<NormalizedTurn> = serde_json::from_str(r###"[{"completeness":"complete","ended_at":null,"logical_turn_key":"turn-0","ordinal":0,"records":[{"kind":"user","text":"fixed synthetic prompt"},{"kind":"assistant","text":"fixed synthetic answer"}],"started_at":null}]"###).unwrap();
            assert_eq!(
                source_digest(&turns),
                "f872e06898f3bb4bcde68e27ac1eaeef2e6109d8faa8d69edea38e69965ca5af"
            );
            assert_eq!(
                source_digest_for_coverage_versions(&turns, &vec![1; turns.len()]).unwrap(),
                "f872e06898f3bb4bcde68e27ac1eaeef2e6109d8faa8d69edea38e69965ca5af"
            );
        }
        {
            let turns: Vec<NormalizedTurn> = serde_json::from_str(r###"[{"completeness":"incomplete","ended_at":2,"logical_turn_key":"turn-0","ordinal":0,"records":[{"kind":"user","text":"固定输入 / newline\nNUL\u0000 / quote\" / slash\\"}],"started_at":1},{"completeness":"complete","ended_at":4,"logical_turn_key":"turn-1","ordinal":1,"records":[{"kind":"assistant","text":"second turn"}],"started_at":3}]"###).unwrap();
            assert_eq!(
                source_digest(&turns),
                "03cbc1dcd2da33de8e9d023479d4417a7bb2f6d3e4af4fd5603f7c0f45c8b685"
            );
            assert_eq!(
                source_digest_for_coverage_versions(&turns, &vec![1; turns.len()]).unwrap(),
                "03cbc1dcd2da33de8e9d023479d4417a7bb2f6d3e4af4fd5603f7c0f45c8b685"
            );
        }
        {
            let turns: Vec<NormalizedTurn> = serde_json::from_str(r###"[{"completeness":"incomplete","ended_at":456,"logical_turn_key":"turn-0","ordinal":7,"records":[{"kind":"user","text":"fixed synthetic prompt"},{"kind":"assistant","text":"fixed synthetic answer"}],"started_at":123}]"###).unwrap();
            assert_eq!(
                source_digest(&turns),
                "f872e06898f3bb4bcde68e27ac1eaeef2e6109d8faa8d69edea38e69965ca5af"
            );
            assert_eq!(
                source_digest_for_coverage_versions(&turns, &vec![1; turns.len()]).unwrap(),
                "f872e06898f3bb4bcde68e27ac1eaeef2e6109d8faa8d69edea38e69965ca5af"
            );
        }
        assert_eq!(
            import_identity_id_for_parts(
                AgentKind::OpenCode,
                "cov05-fixed-provider",
                "opencode_export",
                "fixed/source/locator",
                1
            ),
            "import-9af690cf9308441630bd4fc4d8cce7070569e5d243209a87c53557bb4a50df1d"
        );
        assert_eq!(
            import_identity_id_for_parts(
                AgentKind::OpenCode,
                "cov05-fixed-provider",
                "opencode_export",
                "source/hmac-v2/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                2
            ),
            "import-87265e9c2d8df11c675cc3105be18c323bfa73d5d1497e5bbbb340c7fdf2981b"
        );
        assert_eq!(contiguous_next_ordinal(&BTreeSet::from([])).unwrap(), 0);
        assert_eq!(contiguous_next_ordinal(&BTreeSet::from([0])).unwrap(), 1);
        assert_eq!(contiguous_next_ordinal(&BTreeSet::from([0, 2])).unwrap(), 1);
        assert_eq!(
            contiguous_next_ordinal(&BTreeSet::from([0, 1, 2])).unwrap(),
            3
        );
        assert_eq!(contiguous_next_ordinal(&BTreeSet::from([1])).unwrap(), 0);
        assert_eq!(
            contiguous_next_ordinal(&BTreeSet::from([0, 1, 3])).unwrap(),
            2
        );
        let turn = cov2_test_turn("shared", 0);
        let v1 = source_digest_for_coverage_versions(std::slice::from_ref(&turn), &[1]).unwrap();
        let v2 = source_digest_for_coverage_versions(std::slice::from_ref(&turn), &[2]).unwrap();
        assert_ne!(v1, v2);
        let mixed = vec![turn.clone(), cov2_test_turn("second", 1)];
        assert_ne!(
            source_digest_for_coverage_versions(&mixed, &[1, 2]).unwrap(),
            source_digest_for_coverage_versions(&mixed, &[2, 1]).unwrap()
        );
        assert!(source_digest_for_coverage_versions(&mixed, &[1]).is_err());
        assert!(source_digest_for_coverage_versions(&mixed, &[1, 3]).is_err());
        #[cfg(unix)]
        {
            let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
            let storage = root.join(util::ROOT_DIR);
            let first = import_prepared_with_coverage_versions(
                &conn,
                &storage,
                cov2_import_test_request(&root, &scope, vec![turn.clone()]),
                &[1],
                Duration::from_secs(30),
                Some(&native_import_test_helper_program()),
            )
            .await
            .unwrap();
            assert_eq!(first.checkpoints_written, 1);
            let request = cov2_import_test_request(&root, &scope, vec![turn]);
            let versions = BTreeMap::from([("shared".to_owned(), CoverageSchemaVersion::V2)]);
            assert!(
                committed_source_ordinals_for_coverage(
                    &conn,
                    &request,
                    Some(&versions),
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .unwrap()
                .is_empty()
            );
            assert_eq!(
                committed_source_ordinals(&conn, &request, Instant::now() + Duration::from_secs(5))
                    .await
                    .unwrap(),
                BTreeSet::from([0])
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn cov2_stale_marker_keeps_cursor_unfinished() {
        use super::super::capture::checkpoint::test_support::{self, CheckpointFaultPoint};
        let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let before_traces = cov2_traces_head(&conn).await;
        test_support::fail_once_at(CheckpointFaultPoint::StaleMarkerGeneration);
        let error = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            cov2_import_test_request(&root, &scope, vec![cov2_test_turn("stale", 0)]),
            &[2],
            Duration::from_secs(30),
            Some(&native_import_test_helper_program()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            test_support::armed_fault(),
            None,
            "actual store fault must fire"
        );
        assert!(
            format!("{error:#}").contains("StaleMarkerGeneration"),
            "expected stale-marker rejection: {error:#}"
        );
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='committed' OR next_ordinal<>0").await,0);
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0
        );
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state='catalog_committed'"
            )
            .await,
            0
        );
        assert_eq!(
            cov2_traces_head(&conn).await,
            before_traces,
            "initial traces ref must not advance"
        );
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
            0
        );
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_traces_inflight'"
            )
            .await,
            1,
            "takeover marker must survive old owner cleanup"
        );
    }

    /// PI-01 made `pi` a repository identity; PI-10 adds it to
    /// `IMPORTABLE_AGENT_KINDS`, so the prepared-import entries no longer
    /// refuse it at the kind gate. On Unix a `pi` request imports one
    /// checkpoint through the descriptor helper, and a second import of the
    /// same logical turn writes none.
    #[tokio::test]
    async fn pi_role_prepared_import_entries_accept_pi() {
        assert!(is_importable_kind(AgentKind::Pi));
        #[cfg(unix)]
        {
            let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
            let pi_request = || {
                let mut request =
                    cov2_import_test_request(&root, &scope, vec![cov2_test_turn("pi", 0)]);
                request.agent_kind = AgentKind::Pi;
                request.provider_session_id = "pi-accepted".to_owned();
                request.session_id = "pi__pi-accepted".to_owned();
                request
            };
            let storage = root.join(util::ROOT_DIR);
            let imported = import_prepared_with_coverage_versions(
                &conn,
                &storage,
                pi_request(),
                &[2],
                Duration::from_secs(30),
                Some(&native_import_test_helper_program()),
            )
            .await
            .unwrap();
            assert_eq!(imported.checkpoints_written, 1);
            let replayed = import_prepared_with_coverage_versions(
                &conn,
                &storage,
                pi_request(),
                &[2],
                Duration::from_secs(30),
                Some(&native_import_test_helper_program()),
            )
            .await
            .unwrap();
            assert_eq!(replayed.checkpoints_written, 0);
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_session WHERE agent_kind = 'pi'"
                )
                .await,
                1
            );
        }
    }

    /// PI-10 AC5: a `pi` import that crashes after binding its first turn
    /// (the coordinator's `after_bind` injection skips every cleanup, as a
    /// killed process would) leaves its identity `writing` and commits
    /// nothing. Once the crashed owner's lease has lapsed, the retry takes
    /// the identity over through the same claim store, commits each turn
    /// exactly once, and leaves no in-flight marker; a replay commits nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn pi_explicit_import_recovers_after_a_crash() {
        let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let pi_request = || {
            let mut request = cov2_import_test_request(
                &root,
                &scope,
                vec![cov2_test_turn("pi-0", 0), cov2_test_turn("pi-1", 1)],
            );
            request.agent_kind = AgentKind::Pi;
            request.provider_session_id = "pi-recovered".to_owned();
            request.session_id = "pi__pi-recovered".to_owned();
            request
        };
        let helper = native_import_test_helper_program();
        let import = || {
            import_prepared_with_coverage_versions(
                &conn,
                &storage,
                pi_request(),
                &[2, 2],
                Duration::from_secs(30),
                Some(&helper),
            )
        };
        let crashed = with_import_failpoint("after_bind", import())
            .await
            .expect_err("the injected crash");
        assert!(
            crashed
                .chain()
                .any(|cause| cause.to_string().contains("after_bind")),
            "{crashed:#}"
        );
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='writing'"
            )
            .await,
            1,
            "the crash keeps its ownership for recovery"
        );
        let checkpoints = || cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint");
        let committed_before = checkpoints().await;
        // Stands in for the time a crashed owner's lease takes to lapse.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE agent_import_identity SET lease_expires_at = 0".to_owned(),
        ))
        .await
        .unwrap();
        let recovered = import().await.unwrap();
        assert_eq!(
            committed_before + i64::try_from(recovered.checkpoints_written).unwrap(),
            2,
            "every turn is committed exactly once"
        );
        assert_eq!(checkpoints().await, 2);
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='committed'"
            )
            .await,
            1
        );
        assert_eq!(
            cov2_count(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_traces_inflight'"
            )
            .await,
            0,
            "the recovery retires the crashed owner's marker"
        );
        let replayed = import().await.unwrap();
        assert_eq!(replayed.checkpoints_written, 0);
        assert_eq!(checkpoints().await, 2);
    }

    /// PI-10: Pi's retained thinking is grouped by turn in time linear in
    /// the blocks, so a session with tens of thousands of turns (within Pi's
    /// 16 MiB cap) is grouped well inside its deadline. Each turn's report
    /// holds only its own block; a block naming no stored turn goes with the
    /// first turn.
    #[tokio::test]
    async fn pi_retained_thinking_groups_many_turns_in_linear_time() {
        const TURNS: usize = 40_000;
        let (_tmp, _conn, root, scope) = cov2_import_test_setup().await;
        let turns = (0..TURNS)
            .map(|ordinal| cov2_test_turn(&format!("t{ordinal}"), ordinal))
            .collect();
        let mut request = cov2_import_test_request(&root, &scope, turns);
        request.agent_kind = AgentKind::Pi;
        let mut blocks: Vec<serde_json::Value> = (0..TURNS)
            .map(|ordinal| serde_json::json!({"turn_key": format!("t{ordinal}"), "thinking": "x"}))
            .collect();
        blocks.push(serde_json::json!({"turn_key": null, "thinking": "before"}));
        request.redaction_report = serde_json::json!({"pi_retained": blocks});
        let started = Instant::now();
        let reports = PreparedImportReports::new(&request).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "grouping took {:?}",
            started.elapsed()
        );
        // Every turn's group holds exactly its own block (t0 also the stray).
        let (_, by_turn) = reports.pi_turns.as_ref().expect("Pi groups");
        assert_eq!(by_turn.len(), TURNS);
        for ordinal in 0..TURNS {
            let key = format!("t{ordinal}");
            let blocks = &by_turn[&key];
            assert_eq!(blocks.len(), if ordinal == 0 { 2 } else { 1 }, "{key}");
            assert_eq!(blocks[0]["turn_key"], key);
        }
        assert_eq!(by_turn["t0"][1]["thinking"], "before");
        let report_for = |key: &str| -> serde_json::Value {
            serde_json::from_slice(reports.full_for_turn(key).unwrap().bytes()).unwrap()
        };
        let last = report_for("t39999");
        assert_eq!(last["pi_retained"].as_array().unwrap().len(), 1);
        assert_eq!(last["pi_retained"][0]["turn_key"], "t39999");
        assert_eq!(report_for("t0")["pi_retained"].as_array().unwrap().len(), 2);
        assert_eq!(last["pi_retained_blocks"], TURNS + 1);
        assert!(reports.metadata.get("pi_retained").is_none());
    }

    /// PI-29: every prepared-import entry refuses a kind outside
    /// `IMPORTABLE_AGENT_KINDS` before any lease, catalog row, checkpoint or
    /// object is written, on every platform. On Unix the same request as
    /// `codex` imports (through the descriptor helper), so the refusal is the
    /// kind gate's.
    #[tokio::test]
    async fn import_kind_gate_refuses_every_other_kind_before_any_write() {
        let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let refused_request = |kind: AgentKind| {
            let mut request =
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("gate", 0)]);
            request.agent_kind = kind;
            request.provider_session_id = "gate-refused".to_owned();
            request.session_id = format!("{}__gate-refused", capture_provider_name(kind));
            request
        };
        let tables = [
            "agent_session",
            "agent_import_identity",
            "agent_checkpoint",
            "agent_coverage_claim",
            "object_index",
        ];
        let counts = |conn: DatabaseConnection| async move {
            let mut counts = Vec::new();
            for table in tables {
                counts.push(cov2_count(&conn, &format!("SELECT COUNT(*) AS n FROM {table}")).await);
            }
            counts
        };
        let before = counts(conn.clone()).await;
        let before_files = cov2_object_files(&storage);
        let before_traces = cov2_traces_head(&conn).await;
        let refused: Vec<AgentKind> = AgentKind::all()
            .iter()
            .copied()
            .filter(|kind| !is_importable_kind(*kind))
            .collect();
        assert!(
            [
                AgentKind::Cursor,
                AgentKind::Gemini,
                AgentKind::Copilot,
                AgentKind::FactoryAi,
                AgentKind::Grok,
            ]
            .iter()
            .all(|kind| refused.contains(kind)),
            "{refused:?}"
        );
        for kind in refused {
            let errors = [
                import_prepared(
                    &conn,
                    &storage,
                    refused_request(kind),
                    commit_deadline_after(Duration::from_secs(30)),
                )
                .await
                .map(|_| ())
                .unwrap_err(),
                import_prepared_with_retention(
                    &conn,
                    &storage,
                    refused_request(kind),
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    None,
                    None,
                    None,
                )
                .await
                .map(|_| ())
                .unwrap_err(),
                import_prepared_with_coverage_versions(
                    &conn,
                    &storage,
                    refused_request(kind),
                    &[2],
                    Duration::from_secs(30),
                    None,
                )
                .await
                .map(|_| ())
                .unwrap_err(),
            ];
            for error in errors {
                assert!(
                    error.to_string().contains(&format!(
                        "agent kind '{}' cannot be imported (prepared imports accept claude_code, codex, opencode, hermes, or pi); nothing was written",
                        kind.as_db_str()
                    )),
                    "{kind:?}: {error:#}"
                );
            }
        }
        assert_eq!(counts(conn.clone()).await, before);
        assert_eq!(cov2_object_files(&storage), before_files);
        assert_eq!(cov2_traces_head(&conn).await, before_traces);

        #[cfg(unix)]
        {
            let imported = import_prepared_with_coverage_versions(
                &conn,
                &storage,
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("gate", 0)]),
                &[2],
                Duration::from_secs(30),
                Some(&native_import_test_helper_program()),
            )
            .await
            .unwrap();
            assert_eq!(imported.checkpoints_written, 1);
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn cov2_failed_claim_writes_no_import_artifact() {
        let (_tmp, conn, root, scope) = cov2_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let before_traces = cov2_traces_head(&conn).await;
        let before_files = cov2_object_files(&storage);
        let before = cov2_count(&conn, "SELECT COUNT(*) AS n FROM object_index").await;
        conn.execute_raw(Statement::from_string(conn.get_database_backend(),
            "CREATE TRIGGER cov2_fail_claim_bind BEFORE UPDATE ON agent_coverage_claim
             WHEN NEW.attempt_checkpoint_id IS NOT NULL BEGIN SELECT RAISE(ABORT, 'cov2_claim_binding_failed'); END".to_owned())).await.unwrap();
        let error = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            cov2_import_test_request(&root, &scope, vec![cov2_test_turn("failed", 0)]),
            &[2],
            Duration::from_secs(30),
            Some(&native_import_test_helper_program()),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("cov2_claim_binding_failed"),
            "must reach actual claim SQL fault: {error:#}"
        );
        for query in [
            "SELECT COUNT(*) AS n FROM agent_checkpoint",
            "SELECT COUNT(*) AS n FROM agent_coverage_revision",
            "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_traces_inflight'",
            "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='committed' OR next_ordinal<>0",
        ] {
            assert_eq!(cov2_count(&conn, query).await, 0, "{query}");
        }
        assert_eq!(
            cov2_count(&conn, "SELECT COUNT(*) AS n FROM object_index").await,
            before
        );
        assert_eq!(
            cov2_traces_head(&conn).await,
            before_traces,
            "failed claim must not advance initial traces ref"
        );
        assert_eq!(
            cov2_object_files(&storage),
            before_files,
            "failed claim must construct no object"
        );
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER cov2_fail_claim_bind".to_owned(),
        ))
        .await
        .unwrap();
        let retry = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            cov2_import_test_request(&root, &scope, vec![cov2_test_turn("failed", 0)]),
            &[2],
            Duration::from_secs(30),
            Some(&native_import_test_helper_program()),
        )
        .await
        .unwrap();
        assert_eq!(retry.checkpoints_written, 1);
        assert_eq!(cov2_count(&conn,"SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='committed' AND next_ordinal=1").await,1);
    }

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

    #[cfg(unix)]
    fn native_import_test_request(
        root: &Path,
        scope: &CaptureScope,
        turn_count: usize,
    ) -> ImportRequest {
        use crate::internal::ai::observed_agents::{
            SemanticRecord, reasoning::carrier_test_support as native,
        };

        let commitment = format!("source/hmac-v2/{}", "b".repeat(64));
        let mut snapshot = crate::internal::ai::capture::snapshot::carrier_test_snapshot(
            &native::NONE,
            b"synthetic ordinary import source".to_vec(),
        )
        .safe_projection();
        assert!(snapshot.bind_source_commitment(commitment.clone()));
        let turns = (0..turn_count)
            .map(|ordinal| NormalizedTurn {
                logical_turn_key: format!("synthetic-turn-{ordinal}"),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: vec![SemanticRecord::User {
                    text: format!("ordinary turn {ordinal}"),
                }],
            })
            .collect::<Vec<_>>();
        let turn_boundaries = turns
            .iter()
            .map(|turn| {
                (
                    turn.logical_turn_key.clone(),
                    TurnBoundary {
                        started_at: 100 + turn.ordinal as i64,
                        ended_at: 100 + turn.ordinal as i64,
                    },
                )
            })
            .collect();
        ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: native::SOURCE.to_owned(),
            session_id: native::CAPTURE.to_owned(),
            source_kind: "file".to_owned(),
            source_id: commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: source_digest(&turns),
            started_at: 100,
            ended_at: 99 + turn_count as i64,
            session_state: "stopped".to_owned(),
            stopped_at: Some(99 + turn_count as i64),
            working_dir: root.to_path_buf(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_owned(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: commitment,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: Some(snapshot),
            turn_boundaries,
            turns,
        }
    }

    #[cfg(unix)]
    async fn native_import_test_setup()
    -> (tempfile::TempDir, DatabaseConnection, PathBuf, CaptureScope) {
        let dir = tempfile::tempdir().expect("create native import repository");
        crate::utils::test::setup_with_new_libra_in(dir.path()).await;
        let root = dir
            .path()
            .canonicalize()
            .expect("canonicalize native import root");
        let storage = root
            .join(util::ROOT_DIR)
            .canonicalize()
            .expect("canonicalize native import storage");
        let conn = sea_orm::Database::connect(format!(
            "sqlite://{}",
            storage.join(util::DATABASE).display()
        ))
        .await
        .expect("connect native import database");
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("resolve native import capture scope");
        crate::internal::ai::capture::key::load_capture_dedup_secret_with_mutation_deadline(
            &storage, None,
        )
        .expect("create native import authentication key");
        (dir, conn, root, scope)
    }

    #[cfg(unix)]
    async fn native_import_test_identity_kinds(conn: &DatabaseConnection) -> Vec<String> {
        use crate::internal::{
            ai::observed_agents::reasoning::carrier_test_support as native,
            metadata::{MetadataKv, MetadataScope, ReasoningIdentitySessionMap},
        };

        let value = MetadataKv::get_with_conn(
            conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
        )
        .await
        .expect("read private reasoning identity")
        .expect("private reasoning identity exists")
        .value;
        let value: serde_json::Value =
            serde_json::from_str(&value).expect("decode private reasoning identity envelope");
        let mut kinds = value["records"]
            .as_object()
            .expect("private identity records")
            .values()
            .map(|record| {
                record["body"]["kind"]
                    .as_str()
                    .expect("private identity kind")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        kinds.sort();
        kinds
    }

    #[cfg(unix)]
    async fn native_import_test_count(conn: &DatabaseConnection, query: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            query.to_owned(),
        ))
        .await
        .expect("query native import state")
        .expect("native import count row")
        .try_get_by::<i64, _>("n")
        .expect("decode native import count")
    }

    #[cfg(unix)]
    async fn native_import_test_replay_request(
        conn: &DatabaseConnection,
        root: &Path,
        scope: &CaptureScope,
        turn_count: usize,
    ) -> ImportRequest {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;

        let mut request = native_import_test_request(root, scope, turn_count);
        let existing = load_existing_session_ownership_in_scope(
            conn,
            AgentKind::ClaudeCode,
            native::SOURCE,
            scope,
        )
        .await
        .expect("load replay session ownership");
        validate_scoped_prepared_existing_session(&mut request, existing.as_ref())
            .expect("validate replay session ownership");
        request
    }

    #[cfg(unix)]
    fn native_import_test_transport_context(
        challenge: u8,
    ) -> crate::internal::ai::observed_agents::reasoning::ImportTransportContext {
        use crate::internal::ai::observed_agents::reasoning::{
            ImportTransportContext, ReasoningProvider, carrier_test_support as native,
        };

        ImportTransportContext::new(
            [challenge; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
            ReasoningProvider::ClaudeCode,
            native::CAPTURE.to_owned(),
            native::SOURCE.to_owned(),
        )
        .expect("construct synthetic import transport context")
    }

    #[cfg(unix)]
    fn native_import_test_verified_batch(
        challenge: u8,
        fields: Vec<(u64, Vec<u8>)>,
    ) -> NativeProvenReasoning {
        use crate::internal::ai::observed_agents::reasoning::{
            ImportTransportKey, carrier_test_support as native, seal_import_transport,
            synthetic_native_batch_for_turns, verify_import_transport,
        };

        let context = native_import_test_transport_context(challenge);
        let parent_key = ImportTransportKey::random().expect("mint private import transport key");
        let child_key = parent_key
            .encode_for_helper()
            .into_key()
            .expect("decode helper-only import transport key");
        let dto = seal_import_transport(
            child_key,
            &context,
            synthetic_native_batch_for_turns(native::CAPTURE, native::SOURCE, fields),
        )
        .expect("seal synthetic native import batch");
        verify_import_transport(parent_key, context, dto)
            .expect("verify synthetic native import batch for parent")
    }

    #[cfg(unix)]
    fn native_import_test_helper_program() -> PathBuf {
        let unit_test_binary =
            std::env::current_exe().expect("locate native import unit test binary");
        // Cargo runs the unit-test binary from `target/debug/deps` on a plain
        // invocation but from a build-script output directory
        // (`target/debug/build/<crate>/<hash>/out`) when the harness is
        // relocated. Walk the ancestors and take the first one that actually
        // contains the built helper instead of assuming a fixed depth.
        for ancestor in unit_test_binary.ancestors().skip(1) {
            let candidate = ancestor.join("libra");
            if candidate.is_file() {
                return candidate;
            }
        }
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            let candidate = Path::new(&manifest).join("target/debug/libra");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!(
            "the real Libra helper binary must be built before this focused unit test: exe={}",
            unit_test_binary.display()
        );
    }

    /// Evidence read back from the shared object store and private identity
    /// map after the production import path consumes an owned native batch.
    #[cfg(unix)]
    pub(crate) struct NativeImportStoreEvidence {
        pub(crate) artifact_bytes: Vec<u8>,
        pub(crate) artifact_oid: String,
        pub(crate) artifact_locator: String,
        pub(crate) original_wire_sha256: [u8; 32],
        pub(crate) source_binding_sha256: [u8; 32],
        pub(crate) issuer_tag: &'static str,
        pub(crate) logical_turn: String,
        pub(crate) identity_kind: crate::internal::metadata::ReasoningIdentityKind,
        pub(crate) identity_slot: String,
        pub(crate) identity_descriptor_mac: String,
        pub(crate) expected_descriptor_mac: String,
        pub(crate) committed_claims: i64,
        pub(crate) checkpoints_written: usize,
    }

    /// Let command unit tests hand their *parent-restored* native value to the
    /// real reservation, checkpoint writer and registered Libra object helper.
    #[cfg(unix)]
    pub(crate) async fn import_verified_native_real_store_for_command_test(
        native: NativeProvenReasoning,
        expected_wire: &[u8],
    ) -> Result<NativeImportStoreEvidence> {
        use crate::internal::{
            ai::{
                checkpoint_reader as reader,
                history::{ReasoningArtifactCheckpointSpec, checkpoint_reasoning_artifact_proofs},
                observed_agents::reasoning::{
                    VerifiedCaptureBinding, borrow_native_identity_bindings,
                    carrier_test_support as native_fixture,
                },
            },
            metadata::{
                MetadataKv, MetadataScope, ReasoningIdentityKind, ReasoningIdentityRecord,
                ReasoningIdentitySessionMap,
            },
        };

        let binding = VerifiedCaptureBinding::from_validated_capture_context(
            &native,
            native_fixture::CAPTURE,
            "claude_code",
        )?;
        let proof_views = borrow_native_identity_bindings(&native, &binding)?;
        anyhow::ensure!(proof_views.len() == 1, "expected one native proof");
        let proof = &proof_views[0];
        let original_wire_sha256 = *proof.original_wire_sha256();
        let source_binding_sha256 = *proof.source_binding_sha256();
        let issuer_tag = proof.issuer_tag();
        let logical_turn = proof.logical_turn().to_owned();
        anyhow::ensure!(
            logical_turn == "synthetic-turn-0",
            "native proof belongs to another logical turn"
        );
        let artifact_params = native.artifact_params(&binding)?;
        anyhow::ensure!(artifact_params.len() == 1, "expected one native artifact");
        anyhow::ensure!(
            artifact_params[0].bytes.as_bytes() == expected_wire,
            "verified native bytes differ from the expected wire"
        );
        let expected_wire_sha256: [u8; 32] = Sha256::digest(expected_wire).into();
        anyhow::ensure!(
            original_wire_sha256 == expected_wire_sha256,
            "native proof digest differs from the expected wire"
        );
        let expected_locator = artifact_params[0].locator.clone();

        let (_dir, conn, root, scope) = native_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let request = native_import_test_request(&root, &scope, 1);
        let deadline = commit_deadline_after(Duration::from_secs(60));
        let disposition = ReasoningSourceDisposition::NativeProven(native);
        let plan = prepare_reasoning_coverage_plan_from_disposition(
            &disposition,
            ReasoningIdentityPreparation {
                conn: &conn,
                scope: &scope,
                storage_path: &storage,
                authorized_root: &root,
                session_id: &request.session_id,
                provider: request.agent_kind.as_db_str(),
                turns: &request.turns,
                deadline: deadline.monotonic(),
            },
        )
        .await?
        .context("native command test has no reasoning coverage plan")?;
        let turn_identity = plan
            .turn(&logical_turn)
            .context("native command test has no logical-turn identity")?;
        anyhow::ensure!(
            turn_identity.kind() == ReasoningIdentityKind::Nonempty,
            "native command test must produce a nonempty identity"
        );
        let identity_slot = turn_identity.slot().to_owned();
        let expected_descriptor_mac = turn_identity.descriptor_mac().to_owned();
        let ReasoningSourceDisposition::NativeProven(native) = disposition else {
            return Err(anyhow!(
                "native command test lost its verified reasoning batch"
            ));
        };

        let imported = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &conn,
                &storage,
                request,
                deadline,
                super::super::subagent_content::SubagentDiscovery::default(),
                Some(native),
            ),
        )
        .await
        .context("import parent-restored native reasoning through the real store")?;
        anyhow::ensure!(
            imported.summary.checkpoints_written == 1,
            "native command test did not write exactly one checkpoint"
        );

        let checkpoints = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT checkpoint_id, tree_oid FROM agent_checkpoint WHERE session_id = ?",
                [native_fixture::CAPTURE.into()],
            ))
            .await
            .context("read native command test checkpoint")?;
        anyhow::ensure!(checkpoints.len() == 1, "expected one stored checkpoint");
        let checkpoint_id: String = checkpoints[0].try_get_by("checkpoint_id")?;
        let tree_oid: String = checkpoints[0].try_get_by("tree_oid")?;
        let artifact_proofs = checkpoint_reasoning_artifact_proofs(
            &storage,
            &[ReasoningArtifactCheckpointSpec {
                checkpoint_id: checkpoint_id.clone(),
                tree_oid: tree_oid.clone(),
            }],
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .context("verify committed native artifact objects")?;
        let artifact_proof = artifact_proofs
            .first()
            .and_then(|checkpoint| checkpoint.artifacts.as_ref())
            .and_then(|artifacts| (artifacts.len() == 1).then(|| &artifacts[0]))
            .context("committed checkpoint lacks one proven native artifact")?;
        let artifact_oid = artifact_proof.oid.clone();
        let parsed_oid = crate::internal::object_format::parse_repo_oid(&artifact_oid)?;
        let (artifact_bytes, truncated) = reader::read_checkpoint_object_bounded(
            &storage,
            &parsed_oid,
            u64::try_from(expected_wire.len())?,
        )?;
        anyhow::ensure!(
            !truncated && artifact_bytes == expected_wire,
            "shared object store changed native ciphertext bytes"
        );
        anyhow::ensure!(
            artifact_proof.sha256 == hex::encode(original_wire_sha256)
                && artifact_proof.byte_len == expected_wire.len() as u64,
            "artifact lifecycle proof differs from native proof bytes"
        );

        let root_tree =
            reader::read_tree_object(&storage, &tree_oid).map_err(|error| anyhow!(error))?;
        let checkpoint_tree =
            reader::subtree(&storage, &root_tree, "checkpoint").map_err(|error| anyhow!(error))?;
        let (prefix, rest) = checkpoint_id
            .get(..2)
            .zip(checkpoint_id.get(2..))
            .context("stored checkpoint id has no tree path")?;
        let prefix_tree =
            reader::subtree(&storage, &checkpoint_tree, prefix).map_err(|error| anyhow!(error))?;
        let leaf_tree =
            reader::subtree(&storage, &prefix_tree, rest).map_err(|error| anyhow!(error))?;
        let manifest_entry = reader::tree_entry(&leaf_tree, "manifest.json")
            .context("stored checkpoint lacks its manifest")?;
        let (manifest_bytes, truncated) = reader::read_checkpoint_object_bounded(
            &storage,
            &manifest_entry.id,
            reader::CHECKPOINT_METADATA_READ_MAX_BYTES,
        )?;
        anyhow::ensure!(!truncated, "stored checkpoint manifest is truncated");
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
        let manifest_artifacts = reader::reasoning_artifact_metadata(&manifest)?;
        anyhow::ensure!(
            manifest_artifacts.len() == 1
                && manifest_artifacts[0].locator == expected_locator
                && manifest_artifacts[0].oid == artifact_oid
                && manifest_artifacts[0].sha256 == artifact_proof.sha256
                && manifest_artifacts[0].byte_len == Some(expected_wire.len() as u64),
            "stored manifest does not bind the native locator and object"
        );

        let map_value = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentReasoningIdentity,
            native_fixture::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
        )
        .await?
        .context("stored checkpoint has no reasoning identity map")?;
        let map = ReasoningIdentitySessionMap::decode(&map_value.value)?;
        map.validate_owner(&scope.repo_id, native_fixture::CAPTURE)?;
        let map_body = ReasoningIdentitySessionMap::authentication_body(
            &scope.repo_id,
            native_fixture::CAPTURE,
            map.records(),
        )?;
        plan.authenticate_record(&conn, &map_body, Some(map.mac()))
            .await
            .context("authenticate stored reasoning identity map")?;
        anyhow::ensure!(
            map.records().len() == 1,
            "expected one stored turn identity"
        );
        let record = map
            .records()
            .get(&identity_slot)
            .context("stored turn identity is missing its authenticated slot")?;
        let record_body = ReasoningIdentityRecord::authentication_body(record.body())?;
        plan.authenticate_record(&conn, &record_body, Some(record.mac()))
            .await
            .context("authenticate stored reasoning identity record")?;
        let identity_kind = record.body().kind;
        let identity_descriptor_mac = record.body().descriptor_mac.clone();
        anyhow::ensure!(
            identity_kind == ReasoningIdentityKind::Nonempty
                && identity_descriptor_mac == expected_descriptor_mac
                && record.body().context.logical_turn_key == identity_slot
                && record.body().context.checkpoint_id == checkpoint_id,
            "stored reasoning identity differs from the verified native plan"
        );
        let committed_claims = native_import_test_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state='catalog_committed'",
        )
        .await;
        anyhow::ensure!(
            committed_claims == 1,
            "native coverage claim was not committed"
        );

        Ok(NativeImportStoreEvidence {
            artifact_bytes,
            artifact_oid,
            artifact_locator: manifest_artifacts[0].locator.clone(),
            original_wire_sha256,
            source_binding_sha256,
            issuer_tag,
            logical_turn,
            identity_kind,
            identity_slot,
            identity_descriptor_mac,
            expected_descriptor_mac,
            committed_claims,
            checkpoints_written: imported.summary.checkpoints_written,
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_real_multi_turn_import_consumes_only_reserved_batches() {
        let (_dir, conn, root, scope) = native_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let batch = native_import_test_verified_batch(
            1,
            vec![
                (0, b"private turn zero".to_vec()),
                (0, b"private second item in turn zero".to_vec()),
                (2, b"private turn two".to_vec()),
            ],
        );
        let imported = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &conn,
                &storage,
                native_import_test_request(&root, &scope, 3),
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                Some(batch),
            ),
        )
        .await
        .expect("write two native turns and one authenticated empty turn");
        assert_eq!(imported.summary.checkpoints_written, 3);
        assert_eq!(imported.summary.skipped_covered, 0);
        let stored_session = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT redaction_report FROM agent_session WHERE session_id = ?",
                [
                    crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE
                        .into(),
                ],
            ))
            .await
            .expect("read imported session redaction report")
            .expect("imported session exists");
        let stored_report: serde_json::Value = serde_json::from_str(
            &stored_session
                .try_get_by::<String, _>("redaction_report")
                .expect("stored redaction report"),
        )
        .expect("decode stored redaction report");
        assert_eq!(
            stored_report["import"]["bytes_scanned"], 0,
            "parent validation must not count the helper-redacted turns twice"
        );
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            3
        );
        assert_eq!(
            native_import_test_identity_kinds(&conn).await,
            ["empty", "nonempty", "nonempty"]
        );
        let identity_before_replay = crate::internal::metadata::MetadataKv::get_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
            crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
            crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
        )
        .await
        .expect("read identity before native replay")
        .expect("identity before native replay")
        .value;
        let revisions_before_replay =
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision")
                .await;
        let objects_before_replay =
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM object_index").await;

        let replay = native_import_test_replay_request(&conn, &root, &scope, 3).await;
        let replayed = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &conn,
                &storage,
                replay,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                Some(native_import_test_verified_batch(
                    2,
                    vec![
                        (0, b"private turn zero".to_vec()),
                        (0, b"private second item in turn zero".to_vec()),
                        (2, b"private turn two".to_vec()),
                    ],
                )),
            ),
        )
        .await
        .expect("replay covered native batches without writing them again");
        assert_eq!(replayed.summary.checkpoints_written, 0);
        assert_eq!(replayed.summary.skipped_covered, 3);
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            3
        );
        let identity_after_replay = crate::internal::metadata::MetadataKv::get_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
            crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
            crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
        )
        .await
        .expect("read identity after native replay")
        .expect("identity after native replay")
        .value;
        assert_eq!(identity_after_replay, identity_before_replay);
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision")
                .await,
            revisions_before_replay,
            "skipped batches must not complete another coverage revision"
        );
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM object_index").await,
            objects_before_replay,
            "skipped batches must not write another artifact object"
        );

        // A complete ordinary prefix has no private reasoning map. Appending
        // the first candidate must not manufacture identity for that legacy
        // covered turn, while a newly written ordinary turn gets Empty.
        let (_prefix_dir, prefix_conn, prefix_root, prefix_scope) =
            native_import_test_setup().await;
        let prefix_storage = prefix_root.join(util::ROOT_DIR);
        let mut ordinary_prefix = native_import_test_request(&prefix_root, &prefix_scope, 1);
        ordinary_prefix.session_state = "active".to_owned();
        ordinary_prefix.stopped_at = None;
        let ordinary = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &prefix_conn,
                &prefix_storage,
                ordinary_prefix,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
            ),
        )
        .await
        .expect("write a complete legacy ordinary prefix");
        assert_eq!(ordinary.summary.checkpoints_written, 1);
        let appended = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &prefix_conn,
                &prefix_storage,
                native_import_test_replay_request(&prefix_conn, &prefix_root, &prefix_scope, 3)
                    .await,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                Some(native_import_test_verified_batch(
                    3,
                    vec![(2, b"first later private candidate".to_vec())],
                )),
            ),
        )
        .await
        .expect("append native source without backfilling a covered ordinary prefix");
        assert_eq!(appended.summary.checkpoints_written, 2);
        assert_eq!(appended.summary.skipped_covered, 1);
        assert_eq!(
            native_import_test_identity_kinds(&prefix_conn).await,
            ["empty", "nonempty"]
        );

        // A stale lease and marker are deliberately present: an invalid
        // mapping must fail before acquire_identity can take over either.
        let (_stale_dir, stale_conn, stale_root, stale_scope) = native_import_test_setup().await;
        let stale_storage = stale_root.join(util::ROOT_DIR);
        let stale_request = native_import_test_request(&stale_root, &stale_scope, 2);
        let stale_identity_id = identity_id(&stale_request);
        stale_conn
            .execute_raw(Statement::from_sql_and_values(
                stale_conn.get_database_backend(),
                "INSERT INTO agent_import_identity (
                    identity_id, agent_kind, provider_session_id, source_kind,
                    source_id, schema_version, next_ordinal, state, owner,
                    lease_expires_at, fence_token, created_at, updated_at,
                    attempt_checkpoint_id, repo_id, worktree_id,
                    workspace_id, workspace_fence, scope_state
                 ) VALUES (?, 'claude_code', ?, 'file', ?, 2, 0, 'leased',
                           'stale-owner', 1, 4, 1, 1, 'stale-checkpoint',
                           ?, ?, ?, ?, 'scoped')",
                [
                    stale_identity_id.clone().into(),
                    stale_request.provider_session_id.clone().into(),
                    stale_request.source_id.clone().into(),
                    stale_scope.repo_id.clone().into(),
                    stale_scope.worktree_id.clone().into(),
                    stale_scope.workspace_id.clone().into(),
                    stale_scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed stale import owner");
        let stale_marker =
            TracesInflightMarker::new(&stale_request.session_id, "stale-checkpoint", 4);
        history::write_traces_inflight_marker(&stale_conn, &stale_marker)
            .await
            .expect("seed stale import marker");
        for (request, batch) in [
            (
                {
                    let mut duplicate = native_import_test_request(&stale_root, &stale_scope, 2);
                    duplicate.turns[1].logical_turn_key = "synthetic-turn-0".to_owned();
                    duplicate
                },
                native_import_test_verified_batch(4, vec![(0, b"duplicate turn".to_vec())]),
            ),
            (
                native_import_test_request(&stale_root, &stale_scope, 2),
                native_import_test_verified_batch(5, vec![(2, b"orphan turn".to_vec())]),
            ),
            (
                {
                    let mut incomplete = native_import_test_request(&stale_root, &stale_scope, 2);
                    incomplete.transcript_snapshot = None;
                    incomplete
                },
                native_import_test_verified_batch(6, vec![(0, b"unbound source".to_vec())]),
            ),
            (
                {
                    let mut unredacted = native_import_test_request(&stale_root, &stale_scope, 2);
                    unredacted.turns[0].records =
                        vec![crate::internal::ai::observed_agents::SemanticRecord::User {
                            text: "AKIAAAAAAAAAAAAAAAAA".to_owned(),
                        }];
                    unredacted.content_digest = source_digest(&unredacted.turns);
                    unredacted
                },
                native_import_test_verified_batch(7, vec![(0, b"unredacted source".to_vec())]),
            ),
        ] {
            assert!(
                import_prepared_with_subagent_discovery(
                    &stale_conn,
                    &stale_storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    Some(batch),
                )
                .await
                .is_err(),
                "duplicate, orphan, incomplete-source, and unredacted inputs must fail before lease takeover"
            );
            let row = stale_conn
                .query_one_raw(Statement::from_sql_and_values(
                    stale_conn.get_database_backend(),
                    "SELECT owner, fence_token, attempt_checkpoint_id
                     FROM agent_import_identity WHERE identity_id = ?",
                    [stale_identity_id.clone().into()],
                ))
                .await
                .expect("read unchanged stale import identity")
                .expect("stale import identity remains");
            assert_eq!(
                row.try_get_by::<String, _>("owner").expect("stale owner"),
                "stale-owner"
            );
            assert_eq!(
                row.try_get_by::<i64, _>("fence_token")
                    .expect("stale fence"),
                4
            );
            assert_eq!(
                row.try_get_by::<String, _>("attempt_checkpoint_id")
                    .expect("stale marker binding"),
                "stale-checkpoint"
            );
            assert!(
                crate::internal::metadata::MetadataKv::get_with_conn(
                    &stale_conn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    &stale_request.session_id,
                    "stale-checkpoint",
                )
                .await
                .expect("read unchanged stale marker")
                .is_some(),
                "preflight rejection must not retire the stale marker"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_real_multi_turn_import_failure_keeps_committed_progress() {
        use crate::internal::ai::observed_agents::reasoning::{
            ImportTransportKey, carrier_test_support as native, seal_import_transport,
            synthetic_native_batch_for_turns, verify_import_transport,
        };

        let (_dir, conn, root, scope) = native_import_test_setup().await;
        let storage = root.join(util::ROOT_DIR);
        let first = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            with_import_failpoint(
                "after_catalog_commit",
                import_prepared_with_subagent_discovery(
                    &conn,
                    &storage,
                    native_import_test_request(&root, &scope, 2),
                    commit_deadline_after(Duration::from_secs(30)),
                    super::super::subagent_content::SubagentDiscovery::default(),
                    Some(native_import_test_verified_batch(
                        7,
                        vec![
                            (0, b"private turn zero".to_vec()),
                            (1, b"private turn one".to_vec()),
                        ],
                    )),
                ),
            ),
        )
        .await
        .expect_err("inject a failure after the first durable turn");
        let progress = first
            .downcast_ref::<ImportProgressError>()
            .expect("durable prefix must return partial progress");
        assert_eq!(progress.summary.checkpoints_written, 1);
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
        assert_eq!(native_import_test_identity_kinds(&conn).await, ["nonempty"]);
        assert_eq!(
            native_import_test_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state='reserved_import'"
            )
            .await,
            0,
            "the failed owner must release its uncommitted reservation"
        );

        let old_context = native_import_test_transport_context(7);
        let stale_key = ImportTransportKey::random().expect("mint stale transport key");
        let stale_dto = seal_import_transport(
            stale_key
                .encode_for_helper()
                .into_key()
                .expect("decode stale helper key"),
            &old_context,
            synthetic_native_batch_for_turns(
                native::CAPTURE,
                native::SOURCE,
                vec![(1, b"private turn one".to_vec())],
            ),
        )
        .expect("seal stale synthetic frame");
        assert!(
            verify_import_transport(
                ImportTransportKey::random().expect("mint retry transport key"),
                native_import_test_transport_context(8),
                stale_dto,
            )
            .is_err(),
            "a new retry challenge and key must reject an old-context frame"
        );

        let replay = native_import_test_replay_request(&conn, &root, &scope, 2).await;
        let completed = crate::internal::ai::authorized_read::with_test_helper_program(
            native_import_test_helper_program(),
            import_prepared_with_subagent_discovery(
                &conn,
                &storage,
                replay,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                Some(native_import_test_verified_batch(
                    8,
                    vec![
                        (0, b"private turn zero".to_vec()),
                        (1, b"private turn one".to_vec()),
                    ],
                )),
            ),
        )
        .await
        .expect("retry must commit only the remaining native turn");
        assert_eq!(completed.summary.checkpoints_written, 1);
        assert_eq!(completed.summary.skipped_covered, 1);
        assert_eq!(
            native_import_test_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            2
        );
        assert_eq!(
            native_import_test_identity_kinds(&conn).await,
            ["nonempty", "nonempty"]
        );
    }

    #[cfg(unix)]
    #[cfg(feature = "test-live-agent")]
    mod codex_stored_import_local_tests {
        use super::*;
        use crate::internal::ai::subagent_content::{
            DiscoveredSubagentContent, SubagentCaptureSummary, SubagentDiscovery,
        };

        #[test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        fn codex_stored_import_keeps_incomplete_and_inflight_partial() {
            let incomplete = SubagentDiscovery::charged_stored_unavailable(719);
            assert!(historical_import_is_partial(0, 0, &incomplete));
            assert!(incomplete.sources.is_empty());
            assert_eq!(incomplete.bytes_read, 719);
            let warning_only = SubagentDiscovery {
                warning: Some("legacy unavailable".into()),
                ..Default::default()
            };
            assert!(!historical_import_is_partial(0, 0, &warning_only));
            let mut busy = SubagentCaptureSummary::default();
            busy.skipped_inflight = 1;
            assert!(historical_child_capture_is_partial(AgentKind::Codex, &busy));
            assert!(!historical_child_capture_is_partial(
                AgentKind::ClaudeCode,
                &busy
            ));
            assert!(!historical_child_capture_is_partial(
                AgentKind::Codex,
                &SubagentCaptureSummary::default()
            ));
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_shared_late_child_finalizer_keeps_progress() {
            let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
            let storage = root.join(util::ROOT_DIR);
            let request_for = || {
                let mut request = cov2_import_test_request(
                    &root,
                    &scope,
                    vec![cov2_test_turn("shared-parent", 0)],
                );
                request.agent_kind = AgentKind::ClaudeCode;
                request.session_id = build_ai_session_id("claude", &request.provider_session_id);
                request
            };
            let program = crate::utils::test::real_libra_helper_binary();
            super::super::super::authorized_read::with_test_helper_program(
                program.clone(),
                import_prepared_with_subagent_discovery(
                    &conn,
                    &storage,
                    request_for(),
                    commit_deadline_after(Duration::from_secs(30)),
                    SubagentDiscovery::default(),
                    None,
                ),
            )
            .await
            .unwrap();
            let mut request = request_for();
            let existing = load_existing_session_ownership_in_scope(
                &conn,
                request.agent_kind,
                &request.provider_session_id,
                &scope,
            )
            .await
            .unwrap();
            validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
            let child = DiscoveredSubagentContent::fixture("claude_code", "project/session/subagents/child.jsonl",
                br#"{"type":"assistant","message":{"id":"local-late-child","role":"assistant","content":[{"type":"text","text":"SHARED_LATE_CHILD_BODY"}]}}
"#, None);
            let error = super::super::super::authorized_read::with_test_helper_program(
                program,
                with_import_failpoint(
                    "before_finalize_identity",
                    import_prepared_with_subagent_discovery(
                        &conn,
                        &storage,
                        request,
                        commit_deadline_after(Duration::from_secs(30)),
                        SubagentDiscovery {
                            sources: vec![child],
                            ..Default::default()
                        },
                        None,
                    ),
                ),
            )
            .await
            .expect_err("late child must remain visible after finalizer failure");
            let progress = error.downcast_ref::<ImportProgressError>().unwrap();
            assert_eq!(progress.summary.skipped_covered, 1);
            assert_eq!(progress.subagent_checkpoints_written, 1);
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope='subagent'"
                )
                .await,
                1
            );
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='partial'"
                )
                .await,
                1
            );
            assert!(
                error
                    .chain()
                    .any(|cause| cause.to_string().contains("before_finalize_identity"))
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_error_wrapper_keeps_parent_only_durable_count() {
            let (_tmp, _conn, root, scope) = cov2_runtime_store_setup().await;
            let request =
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("parent-progress", 0)]);
            let lease = ImportLease {
                identity_id: "local-id".into(),
                owner: "local-owner".into(),
                fence_token: 7,
            };
            let error = import_error_with_child_progress(
                anyhow!("local parent reader failure"),
                &request,
                &lease,
                &coverage_gate::LiveReservationOutcome::default(),
                2,
                0,
            );
            let progress = error.downcast_ref::<ImportProgressError>().unwrap();
            assert_eq!(progress.summary.checkpoints_written, 2);
            assert_eq!(progress.subagent_checkpoints_written, 0);
            assert!(
                error
                    .chain()
                    .any(|cause| cause.to_string() == "local parent reader failure")
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_child_usage_import_errors_keep_independent_parent_and_child_progress() {
            for failpoint in ["child", "after_bind", "before_parent_commit"] {
                let parent_failure = failpoint != "child";
                let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
                let storage = root.join(util::ROOT_DIR);
                let request = cov2_import_test_request(
                    &root,
                    &scope,
                    vec![cov2_test_turn("stored-parent", 0)],
                );
                let bytes = format!(
                    "{}\n{}\n",
                    serde_json::json!({"type":"session_meta","payload":{"id":"stored-child",
                        "session_id":"stored-root","parent_thread_id":request.provider_session_id,"cwd":root}}),
                    serde_json::json!({"type":"response_item","payload":{"type":"reasoning",
                        "summary":[{"type":"summary_text","text":"INDEPENDENT_CHILD_THINKING"}]}})
                );
                let child = DiscoveredSubagentContent::fixture_codex_child(
                    "stored-root",
                    &request.provider_session_id,
                    "stored-child",
                    &request.session_id,
                    bytes.as_bytes(),
                );
                if !parent_failure {
                    conn.execute_unprepared("CREATE TRIGGER fail_child_revision BEFORE INSERT ON agent_subagent_content_revision BEGIN SELECT RAISE(ABORT, 'local child failure'); END").await.unwrap();
                }
                let program = crate::utils::test::real_libra_helper_binary();
                let operation = import_prepared_with_subagent_discovery(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    SubagentDiscovery {
                        sources: vec![child],
                        bytes_read: bytes.len() as u64,
                        ..Default::default()
                    },
                    None,
                );
                let error = super::super::super::authorized_read::with_test_helper_program(
                    program,
                    async {
                        if parent_failure {
                            with_import_failpoint(failpoint, operation).await
                        } else {
                            operation.await
                        }
                    },
                )
                .await
                .expect_err("injected failure must preserve progress");
                let progress = error
                    .downcast_ref::<ImportProgressError>()
                    .expect("typed progress");
                assert_eq!(
                    progress.summary.checkpoints_written,
                    usize::from(!parent_failure)
                );
                assert_eq!(
                    progress.subagent_checkpoints_written,
                    usize::from(parent_failure)
                );
                assert!(progress.summary.partial);
                assert_eq!(
                    cov2_count(
                        &conn,
                        "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='partial'"
                    )
                    .await,
                    i64::from(failpoint != "after_bind"),
                    "after_bind deliberately preserves its crash marker; other failures settle partial"
                );
                if failpoint == "after_bind" {
                    assert_eq!(
                        cov2_count(
                            &conn,
                            "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='writing'"
                        )
                        .await,
                        1
                    );
                }
                assert_eq!(
                    cov2_count(
                        &conn,
                        "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope='committed'"
                    )
                    .await,
                    i64::from(!parent_failure)
                );
                assert_eq!(
                    cov2_count(
                        &conn,
                        "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope='subagent'"
                    )
                    .await,
                    i64::from(parent_failure)
                );
                assert!(
                    error
                        .chain()
                        .any(|cause| cause.to_string().contains(if parent_failure {
                            failpoint
                        } else {
                            "local child failure"
                        }))
                );
            }
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_stored_import_zero_sources_keeps_durable_identity_partial() {
            let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
            let storage = root.join(util::ROOT_DIR);
            let request =
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("stored-parent", 0)]);
            let outcome = super::super::super::authorized_read::with_test_helper_program(
                native_import_test_helper_program(),
                import_prepared_with_subagent_discovery(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    SubagentDiscovery::charged_stored_unavailable(719),
                    None,
                ),
            )
            .await
            .expect("partial parent still needs the explicitly registered current helper");
            assert!(outcome.summary.partial);
            assert_eq!(outcome.summary.checkpoints_written, 1);
            assert_eq!(outcome.subagent_checkpoints_written, 0);
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='partial'"
                )
                .await,
                1
            );
            assert_eq!(
                cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
                )
                .await,
                0
            );
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_stored_import_materializes_replays_and_keeps_busy_child_partial() {
            use crate::internal::ai::checkpoint_reader as reader;
            let (_tmp, conn, root, scope) = cov2_runtime_store_setup().await;
            let storage = root.join(util::ROOT_DIR);
            let request =
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("stored-parent", 0)]);
            let mut child_bytes = serde_json::to_vec(&serde_json::json!({
                "type":"session_meta", "payload":{
                    "id":"stored-child", "session_id":"stored-root",
                    "parent_thread_id":"cov2-direct-fixed", "cwd": root,
                },
            }))
            .unwrap();
            child_bytes.push(b'\n');
            child_bytes.extend_from_slice(br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"IMPORTED_CHILD_THINKING"}],"encrypted_content":"stored-opaque","future_field":{"keep":true}}}
"#);
            let child = DiscoveredSubagentContent::fixture_codex_child(
                "stored-root",
                &request.provider_session_id,
                "stored-child",
                &request.session_id,
                &child_bytes,
            );
            let discovery = SubagentDiscovery {
                sources: vec![child],
                bytes_read: child_bytes.len() as u64,
                ..Default::default()
            };
            let program = crate::utils::test::real_libra_helper_binary();
            assert!(
                program.is_file(),
                "build current CLI before operator-local import test"
            );
            for replay in [false, true] {
                let mut request = cov2_import_test_request(
                    &root,
                    &scope,
                    vec![cov2_test_turn("stored-parent", 0)],
                );
                let existing = load_existing_session_ownership_in_scope(
                    &conn,
                    request.agent_kind,
                    &request.provider_session_id,
                    &scope,
                )
                .await
                .unwrap();
                validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
                let outcome = super::super::super::authorized_read::with_test_helper_program(
                    program.clone(),
                    import_prepared_with_subagent_discovery(
                        &conn,
                        &storage,
                        request,
                        commit_deadline_after(Duration::from_secs(30)),
                        discovery.clone(),
                        None,
                    ),
                )
                .await
                .unwrap();
                assert!(!outcome.summary.partial);
                assert_eq!(outcome.summary.checkpoints_written, usize::from(!replay));
                assert_eq!(outcome.subagent_checkpoints_written, usize::from(!replay));
                assert_eq!(
                    cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                    2
                );
                assert_eq!(
                    cov2_count(
                        &conn,
                        "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                    )
                    .await,
                    1
                );
                assert_eq!(cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE link_state='unresolved' AND boundary_checkpoint_id IS NULL").await, 1);
            }
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT checkpoint_id, tree_oid FROM agent_checkpoint WHERE scope='subagent'"
                        .to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            let id: String = row.try_get_by("checkpoint_id").unwrap();
            let tree: String = row.try_get_by("tree_oid").unwrap();
            let wrapped = reader::read_tree_object(&storage, &tree).unwrap();
            let wrapped = reader::subtree(&storage, &wrapped, "checkpoint").unwrap();
            let prefix = reader::subtree(&storage, &wrapped, &id[..2]).unwrap();
            let leaf = reader::subtree(&storage, &prefix, &id[2..]).unwrap();
            let transcript = reader::subtree(&storage, &leaf, "transcript").unwrap();
            let oid = &reader::tree_entry(&transcript, "codex.jsonl").unwrap().id;
            let (persisted, truncated) =
                reader::read_checkpoint_object_bounded(&storage, oid, 4096).unwrap();
            assert!(!truncated);
            assert_eq!(
                persisted, child_bytes,
                "readable thinking, opaque and unknown fields must survive actual import"
            );

            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_subagent_content_claim SET state='reserved',
                 owner='owned-concurrent-child', attempt_digest=current_digest,
                 attempt_checkpoint_id='owned-concurrent-checkpoint',
                 lease_expires_at=?, fence_token=fence_token+1",
                [(Utc::now().timestamp_millis() + 60_000).into()],
            ))
            .await
            .unwrap();
            let mut request =
                cov2_import_test_request(&root, &scope, vec![cov2_test_turn("stored-parent", 0)]);
            let existing = load_existing_session_ownership_in_scope(
                &conn,
                request.agent_kind,
                &request.provider_session_id,
                &scope,
            )
            .await
            .unwrap();
            validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
            let busy = super::super::super::authorized_read::with_test_helper_program(
                program,
                import_prepared_with_subagent_discovery(
                    &conn,
                    &storage,
                    request,
                    commit_deadline_after(Duration::from_secs(30)),
                    discovery,
                    None,
                ),
            )
            .await
            .expect("active child owner must preserve partial parent without takeover");
            assert!(busy.summary.partial);
            assert_eq!(busy.summary.checkpoints_written, 0);
            assert_eq!(busy.subagent_checkpoints_written, 0);
            assert_eq!(
                cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                2
            );
            assert_eq!(
                cov2_count(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_import_identity WHERE state='partial'"
                )
                .await,
                1
            );
            assert_eq!(cov2_count(&conn, "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE state='reserved' AND owner='owned-concurrent-child' AND current_revision=1").await, 1);
        }
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
    #[cfg(unix)]
    async fn reasoning_partial_outcome_reaches_actual_import_handler() {
        use crate::internal::{
            ai::{
                capture::{
                    checkpoint::{
                        ReasoningIdentityPreparation, prepare_reasoning_coverage_plan,
                        reasoning_coverage_test_payload,
                    },
                    key::load_capture_dedup_secret_with_mutation_deadline,
                },
                coverage_gate::{
                    CaptureReservationExecution, ReasoningClaimCommitPlan, ReasoningCoverageInput,
                    reserve_turn_claims_with_reasoning,
                },
                history::{TracesCommitCtx, TracesTxnExtra},
                observed_agents::{SemanticRecord, reasoning::carrier_test_support as native},
            },
            metadata::ReasoningIdentitySessionMap,
        };

        // Real import orchestration and SQLite transactions over synthetic
        // admitted Claude carrier evidence; this is not a provider archive
        // positive and never enables the OpenCode native factory.
        for fresh_count in [0usize, 1, 65] {
            let dir = tempfile::tempdir().unwrap();
            let storage = dir.path().join(util::ROOT_DIR);
            std::fs::create_dir_all(storage.join("objects")).unwrap();
            let conn = crate::internal::db::create_database(
                &storage.join(util::DATABASE).to_string_lossy(),
            )
            .await
            .unwrap();
            crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
                .await
                .unwrap();
            let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir,
                 metadata_json, started_at, last_event_at, sync_revision, repo_id, worktree_id, scope_state)
                 VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
                [native::CAPTURE.into(), native::SOURCE.into(), dir.path().to_string_lossy().into_owned().into(), scope.repo_id.clone().into()],
            )).await.unwrap();
            load_capture_dedup_secret_with_mutation_deadline(&storage, None).unwrap();
            let native_turn = NormalizedTurn {
                logical_turn_key: "synthetic-turn-0".to_owned(),
                ordinal: 0,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: vec![SemanticRecord::User {
                    text: "ordinary parent".to_owned(),
                }],
            };
            let turns = vec![native_turn.clone()];
            let payload =
                reasoning_coverage_test_payload(vec![b"synthetic private identity".to_vec()]);
            let deadline = commit_deadline_after(Duration::from_secs(30));
            let plan = prepare_reasoning_coverage_plan(
                &payload,
                ReasoningIdentityPreparation {
                    conn: &conn,
                    scope: &scope,
                    storage_path: &storage,
                    authorized_root: dir.path(),
                    session_id: native::CAPTURE,
                    provider: "claude_code",
                    turns: &turns,
                    deadline: deadline.monotonic(),
                },
            )
            .await
            .unwrap()
            .unwrap();
            let reserved = reserve_turn_claims_with_reasoning(
                &conn,
                &scope,
                native::CAPTURE,
                &turns,
                "live-before-import",
                1,
                CaptureReservationExecution::live(deadline),
                ReasoningCoverageInput::native(&plan),
            )
            .await
            .unwrap();
            let companion = ReasoningClaimCommitPlan::new(
                LiveClaimCommitPlan {
                    source_channel: "live",
                    session_id: native::CAPTURE.to_owned(),
                    checkpoint_id: "synthetic-live-checkpoint".to_owned(),
                    owner: "live-before-import".to_owned(),
                    parent_commit: None,
                    created_at: 1,
                    now_ms: 1,
                    claims: reserved.ordinary.reserved,
                    import_session: None,
                    import_identity: None,
                    capture_scope: Some(scope.clone()),
                },
                &plan,
            )
            .unwrap();
            let txn = sea_orm::TransactionTrait::begin(&conn).await.unwrap();
            companion
                .apply(
                    &txn,
                    &TracesCommitCtx {
                        commit_hash: "synthetic-live-commit".to_owned(),
                        tree_oid: "t".to_owned(),
                        metadata_blob_oid: "m".to_owned(),
                    },
                )
                .await
                .unwrap();
            txn.commit().await.unwrap();
            let before = MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .unwrap()
            .value;

            let mut import_turns = vec![native_turn];
            for index in 0..fresh_count {
                import_turns.push(NormalizedTurn {
                    logical_turn_key: format!("a-fresh-{index:03}"),
                    ordinal: index + 1,
                    completeness: Completeness::Complete,
                    started_at: None,
                    ended_at: None,
                    records: vec![SemanticRecord::User {
                        text: format!("fresh {index}"),
                    }],
                });
            }
            let commitment = format!("source/hmac-v2/{}", "a".repeat(64));
            let mut request = ImportRequest {
                agent_kind: AgentKind::ClaudeCode,
                provider_session_id: native::SOURCE.to_owned(),
                session_id: native::CAPTURE.to_owned(),
                source_kind: "file".to_owned(),
                source_id: commitment.clone(),
                identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
                content_digest: "synthetic-import-digest".to_owned(),
                started_at: 1,
                ended_at: 1,
                session_state: "active".to_owned(),
                stopped_at: None,
                working_dir: dir.path().to_path_buf(),
                repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_owned(),
                capture_scope: Some(scope),
                source_fingerprint: commitment,
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
                turns: import_turns,
            };
            let existing = load_existing_session_ownership_in_scope(
                &conn,
                AgentKind::ClaudeCode,
                native::SOURCE,
                request.capture_scope.as_ref().unwrap(),
            )
            .await
            .unwrap();
            validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
            let deadline = commit_deadline_after(Duration::from_secs(30));
            let discovery = super::super::subagent_content::SubagentDiscovery::default();
            let result = if fresh_count == 0 {
                import_prepared_with_reasoning_identity(
                    &conn,
                    &storage,
                    request,
                    deadline,
                    discovery,
                    Some(plan.clone()),
                )
                .await
            } else {
                import_prepared_with_subagent_discovery(
                    &conn, &storage, request, deadline, discovery, None,
                )
                .await
            }
            .unwrap();
            assert_eq!(result.summary.partial, fresh_count != 0);
            assert_eq!(result.summary.turns_seen, fresh_count + 1);
            assert_eq!(result.summary.checkpoints_written, 0);
            assert_eq!(
                result.summary.skipped_covered,
                usize::from(fresh_count == 0)
            );
            assert_eq!(result.subagent_checkpoints_written, 0);
            let row = conn.query_one_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "SELECT state, committed_digest, next_ordinal, owner, attempt_checkpoint_id, last_error_code
                 FROM agent_import_identity WHERE identity_id = ?", [result.import_identity_id.into()],
            )).await.unwrap().unwrap();
            let expected_state = if fresh_count == 0 {
                "committed"
            } else {
                "partial"
            };
            assert_eq!(
                row.try_get_by::<String, _>("state").unwrap(),
                expected_state
            );
            assert_eq!(
                row.try_get_by::<Option<String>, _>("committed_digest")
                    .unwrap(),
                (fresh_count == 0).then(|| "synthetic-import-digest".to_owned())
            );
            assert_eq!(
                row.try_get_by::<i64, _>("next_ordinal").unwrap(),
                i64::from(fresh_count == 0)
            );
            assert_eq!(row.try_get_by::<Option<String>, _>("owner").unwrap(), None);
            assert_eq!(
                row.try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                    .unwrap(),
                None
            );
            assert_eq!(
                row.try_get_by::<Option<String>, _>("last_error_code")
                    .unwrap(),
                (fresh_count != 0).then(|| "LBR-AGENT-018".to_owned())
            );
            for (query, expected) in [
                ("SELECT COUNT(*) AS n FROM agent_checkpoint", 1i64),
                ("SELECT COUNT(*) AS n FROM object_index", 0),
                (
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state='reserved_import'",
                    0,
                ),
                (
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state='catalog_committed'",
                    1,
                ),
                ("SELECT COUNT(*) AS n FROM agent_coverage_revision", 1),
                (
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope IN ('agent_traces_inflight', 'agent_capture_pending', 'agent_capture_pending_chunk')",
                    0,
                ),
            ] {
                let n = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        query.to_owned(),
                    ))
                    .await
                    .unwrap()
                    .unwrap()
                    .try_get_by::<i64, _>("n")
                    .unwrap();
                assert_eq!(n, expected, "fresh_count {fresh_count}: {query}");
            }
            let after = MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .unwrap()
            .value;
            assert_eq!(before, after);
            assert!(!after.contains("synthetic private identity"));
            assert_eq!(
                std::fs::read_dir(storage.join("objects")).unwrap().count(),
                0
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_import_reservation_uses_source_identity() {
        use crate::internal::ai::{
            capture_scope::CaptureCommitDeadline,
            coverage_gate::{
                LiveCaptureClaimCommitPlan, LiveClaimCommitPlan, ReasoningCoverageInput,
                tests::{ReasoningFixture, turn},
            },
            history::{TracesCommitCtx, TracesTxnExtra},
            observed_agents::coverage::Completeness,
        };

        let fixture = ReasoningFixture::new().await;
        let turn = turn(
            "synthetic-turn-0",
            "ordinary transcript turn",
            Completeness::Complete,
        );
        let plan = fixture
            .plan(
                std::slice::from_ref(&turn),
                vec![b"synthetic native identity for import".to_vec()],
            )
            .await;
        let first =
            crate::internal::ai::coverage_gate::reserve_import_turn_claims_with_reasoning_until(
                &fixture.conn,
                &fixture.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                std::slice::from_ref(&turn),
                "import-native-owner",
                10,
                CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
                ReasoningCoverageInput::native(&plan),
            )
            .await
            .unwrap();
        assert_eq!(first.skipped_incomplete_reasoning, 0);
        assert_eq!(first.ordinary.reserved.len(), 1);

        let checkpoint = "import-native-checkpoint";
        let companion = LiveCaptureClaimCommitPlan::new(
            LiveClaimCommitPlan {
                source_channel: "import",
                session_id:
                    crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE
                        .to_owned(),
                checkpoint_id: checkpoint.to_owned(),
                owner: "import-native-owner".to_owned(),
                parent_commit: None,
                created_at: 10,
                now_ms: 10,
                claims: first.ordinary.reserved,
                import_session: None,
                import_identity: None,
                capture_scope: Some(fixture.scope.clone()),
            },
            Some(&plan),
        )
        .unwrap();
        let txn = sea_orm::TransactionTrait::begin(&fixture.conn)
            .await
            .unwrap();
        TracesTxnExtra::apply(
            &companion,
            &txn,
            &TracesCommitCtx {
                commit_hash: "import-native-commit".to_owned(),
                tree_oid: "tree".to_owned(),
                metadata_blob_oid: "metadata".to_owned(),
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        let redelivery =
            crate::internal::ai::coverage_gate::reserve_import_turn_claims_with_reasoning_until(
                &fixture.conn,
                &fixture.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                std::slice::from_ref(&turn),
                "import-native-redelivery",
                20,
                CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
                ReasoningCoverageInput::native(&plan),
            )
            .await
            .unwrap();
        assert_eq!(redelivery.skipped_incomplete_reasoning, 0);
        assert_eq!(redelivery.ordinary.skipped_covered, 1);
        assert!(redelivery.ordinary.reserved.is_empty());
    }

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
    fn opencode_import_collects_native_and_classic_directory_declarations() {
        for info in [
            serde_json::json!({"id":"ses_directory", "location":{"directory":"/repo"}}),
            serde_json::json!({"id":"ses_directory", "directory":"/repo"}),
            serde_json::json!({"id":"ses_directory", "cwd":"/repo"}),
            serde_json::json!({"id":"ses_directory", "directory":"/repo", "cwd":"/repo",
                "location":{"directory":"/repo"}}),
        ] {
            let bytes = serde_json::to_vec(&serde_json::json!({"info":info, "messages":[]}))
                .expect("encode OpenCode directory fixture");
            let facts = collect_facts(AgentKind::OpenCode, &bytes, None)
                .expect("collect OpenCode native/classic directory facts");
            assert_eq!(facts.working_dirs, BTreeSet::from([PathBuf::from("/repo")]));
            assert_eq!(
                facts.provider_session_ids,
                BTreeSet::from(["ses_directory".to_owned()])
            );
        }
    }

    #[test]
    fn opencode_import_directory_conflicts_and_repository_scope_are_preserved() {
        let fixture = tempfile::tempdir().expect("create explicit repository-scope fixture");
        let repository = fixture.path().join("repo");
        let foreign = fixture.path().join("foreign");
        for directory in [&repository, &foreign] {
            let storage = directory.join(util::ROOT_DIR);
            std::fs::create_dir_all(&storage).expect("create repository recognition markers");
            // Scope discovery needs only the existing database marker; these
            // fixtures never open a database or invoke an agent/model.
            std::fs::write(storage.join(util::DATABASE), b"").expect("write database marker");
        }
        let repository = repository.canonicalize().expect("canonicalize repository");
        let foreign = foreign
            .canonicalize()
            .expect("canonicalize foreign repository");
        let storage = repository
            .join(util::ROOT_DIR)
            .canonicalize()
            .expect("canonicalize storage");
        let facts_for = |info| {
            let bytes = serde_json::to_vec(&serde_json::json!({"info":info, "messages":[]}))
                .expect("encode explicit directory declarations");
            collect_facts(AgentKind::OpenCode, &bytes, None)
                .expect("collect directory declarations")
        };
        let native = facts_for(serde_json::json!({"location":{"directory":repository}}));
        assert_eq!(
            verified_repo_root(&native, &repository, &storage).expect("native directory accepted"),
            repository
        );
        // Non-string values add no directory facts under the existing filter.
        // A valid classic string remains usable; this does not approve the
        // complete export schema or bypass the earlier source authorization.
        let non_string_native =
            facts_for(serde_json::json!({"directory":repository, "location":{"directory":42}}));
        assert_eq!(
            verified_repo_root(&non_string_native, &repository, &storage)
                .expect("only string directory evidence is considered"),
            repository
        );
        let lexical_alias = facts_for(serde_json::json!({"directory":repository,
            "cwd":repository.join("."), "location":{"directory":repository}}));
        assert_eq!(
            verified_repo_root(&lexical_alias, &repository, &storage)
                .expect("same canonical directory accepted"),
            repository
        );
        for info in [
            serde_json::json!({"directory":repository, "location":{"directory":foreign}}),
            serde_json::json!({"cwd":repository, "location":{"directory":foreign}}),
            serde_json::json!({"directory":repository, "cwd":foreign}),
            serde_json::json!({"location":{"directory":42}}),
            serde_json::json!({"location":{"directory":true}}),
            serde_json::json!({"location":{"directory":null}}),
            serde_json::json!({"location":{"directory":[]}}),
            serde_json::json!({"location":{"directory":{}}}),
            serde_json::json!({}),
        ] {
            let facts = facts_for(info);
            let error = verified_repo_root(&facts, &repository, &storage)
                .expect_err("missing/conflicting directory refused");
            assert!(matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::WorkingDirMissingOrAmbiguous)
            ));
        }
        let foreign_only = facts_for(serde_json::json!({"location":{"directory":foreign}}));
        let error = verified_repo_root(&foreign_only, &repository, &storage)
            .expect_err("foreign native repository refused");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::RepositoryConflict)
        ));
        #[cfg(unix)]
        {
            let alias = fixture.path().join("repo-alias");
            std::os::unix::fs::symlink(&repository, &alias)
                .expect("create canonical directory alias");
            let aliased = facts_for(
                serde_json::json!({"directory":alias, "location":{"directory":repository}}),
            );
            assert_eq!(
                verified_repo_root(&aliased, &repository, &storage)
                    .expect("symlink same-repository alias accepted"),
                repository
            );
        }
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
            // Full-suite scheduling can delay fixture startup beyond one second.
            for _ in 0..500 {
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
            // Only the fixture startup window grows; the reap check is unchanged.
            for _ in 0..500 {
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
            None,
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
            None,
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
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_unready_import_returns_owned_rejection() {
        use crate::internal::ai::observed_agents::{
            live_capture::scoped_test_snapshot_adapter, reasoning::carrier_test_support as fixture,
        };
        let dir = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(dir.path()).await;
        let root = dir.path().canonicalize().unwrap();
        let storage = root.join(".libra").canonicalize().unwrap();
        let bytes = |content: &str| {
            let value = serde_json::json!({"type":"user", "uuid":"user-og16", "sessionId":fixture::SOURCE,
                "cwd":root, "message":{"role":"user","content":content}});
            [serde_json::to_vec(&value).unwrap(), vec![b'\n']].concat()
        };
        for adapter in [
            &fixture::NATIVE,
            &fixture::STRIP,
            &fixture::UNMAPPED,
            &fixture::HELPER,
        ] {
            let snapshot = crate::internal::ai::capture::snapshot::carrier_test_snapshot(
                adapter,
                fixture::CANARY.to_vec(),
            );
            let owning = snapshot
                .try_into_ordinary()
                .expect_err("unready consumer must retain the carrier");
            assert!(owning.rejects_ordinary_source_consumer());
            let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, adapter);
            let source = bytes(std::str::from_utf8(fixture::CANARY).unwrap());
            let error = prepare_import_request(
                AgentKind::ClaudeCode,
                fixture::SOURCE,
                "claude_file",
                "synthetic-source",
                AuthorizedImportContent {
                    bytes: source.clone(),
                    provisional_session_id: fixture::CAPTURE.to_owned(),
                },
                ImportPreparationContext {
                    current_repo_root: &root,
                    current_storage_root: &storage,
                    deadline: Instant::now() + Duration::from_secs(10),
                },
            )
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::SourceAuthorization)
            ));
            let error = prepare_import_projection(
                AgentKind::ClaudeCode,
                import_provider_commitment(fixture::SOURCE),
                source,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::SourceAuthorization)
            ));
            assert!(!format!("{error:#}").contains(std::str::from_utf8(fixture::CANARY).unwrap()));
        }
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::NONE);
        let ordinary = bytes("ordinary compatible message");
        prepare_import_request(
            AgentKind::ClaudeCode,
            fixture::SOURCE,
            "claude_file",
            "synthetic-source",
            AuthorizedImportContent {
                bytes: ordinary.clone(),
                provisional_session_id: fixture::CAPTURE.to_owned(),
            },
            ImportPreparationContext {
                current_repo_root: &root,
                current_storage_root: &storage,
                deadline: Instant::now() + Duration::from_secs(10),
            },
        )
        .expect("old ordinary import remains usable");
        prepare_import_projection(
            AgentKind::ClaudeCode,
            import_provider_commitment(fixture::SOURCE),
            ordinary,
            Instant::now() + Duration::from_secs(10),
        )
        .expect("old ordinary helper projection remains usable");
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_retention_report_tests {
    use sea_orm::TransactionTrait;

    use super::*;

    fn commit_deadline_after(duration: Duration) -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() + duration,
            Utc::now().timestamp_millis() + i64::try_from(duration.as_millis()).unwrap(),
        )
    }

    async fn summary_import(
        conn: &DatabaseConnection,
        storage: &Path,
        request: ImportRequest,
        retained: CodexRetention,
    ) {
        let binary = crate::utils::test::real_libra_helper_binary();
        assert!(
            binary.is_file(),
            "build the Libra binary before this local test"
        );
        super::super::authorized_read::with_test_helper_program(
            binary,
            import_prepared_with_retention(
                conn,
                storage,
                request,
                commit_deadline_after(Duration::from_secs(30)),
                super::super::subagent_content::SubagentDiscovery::default(),
                None,
                None,
                Some(retained),
            ),
        )
        .await
        .unwrap();
    }

    // A pre-CX07 helper DTO with no summary, written through the actual
    // ref/coverage/catalog transaction; no checkpoint hashes are forged.
    fn summary_upgrade_request(
        root: &Path,
        scope: &CaptureScope,
        count: usize,
        with_summary: bool,
    ) -> (ImportRequest, CodexRetention) {
        use crate::internal::ai::{
            capture::snapshot::{
                CaptureSnapshotCompleteness, CaptureSnapshotSource, CaptureSnapshotSourceKind,
            },
            observed_agents::{Redactor, SemanticRecord},
        };
        let turns: Vec<NormalizedTurn> = (0..count)
            .map(|ordinal| NormalizedTurn {
                logical_turn_key: format!("cx07-turn-{ordinal}"),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: vec![
                    SemanticRecord::User {
                        text: format!("question {ordinal}"),
                    },
                    SemanticRecord::Assistant {
                        text: format!("answer {ordinal}"),
                    },
                ],
            })
            .collect();
        let mut lines = vec![
            serde_json::json!({"type":"session_meta","payload":{"id":"cx07-upgrade","cwd":root,"cli_version":"0.162.1"}}),
            serde_json::json!({"type":"turn_context","payload":{"turn_id":"cx07-turn-0","model":"CX07-UPGRADE"}}),
        ];
        let mut slices = BTreeMap::new();
        for ordinal in 0..count {
            lines.push(serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("u{ordinal}"),"role":"user","internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]},"content":[{"type":"input_text","text":format!("question {ordinal}")}]}}));
            let reasoning = serde_json::json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":format!("old readable thinking {ordinal}")}]}});
            slices.insert(ordinal, format!("{reasoning}\n").into_bytes());
            lines.push(reasoning);
            lines.push(serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("a{ordinal}"),"role":"assistant","content":[{"type":"output_text","text":format!("answer {ordinal}")}]}}));
        }
        let raw = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let source = Redactor::new_default().redact(raw.as_bytes()).0;
        let mut dto = super::super::observed_agents::codex_reasoning::CodexRetentionDto::encode(
            &turns, slices,
        )
        .unwrap();
        if with_summary {
            dto = dto.with_summary(
                super::super::observed_agents::session_summary::from_redacted_codex_snapshot(
                    &source,
                    Instant::now() + Duration::from_secs(30),
                ),
            );
        } else {
            assert!(serde_json::to_value(&dto).unwrap().get("summary").is_none());
        }
        let retained = dto
            .decode(
                &turns,
                source.bytes().len(),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        let commitment = format!("source/hmac-v2/{}", "a".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::Codex,
            provider_session_id: "cx07-upgrade".to_owned(),
            session_id: "codex__cx07-upgrade".to_owned(),
            source_kind: "file".to_owned(),
            source_id: commitment.clone(),
            identity_schema_version: IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            content_digest: "caller digest is rederived".to_owned(),
            started_at: 1,
            ended_at: count as i64 + 1,
            session_state: "stopped".to_owned(),
            stopped_at: Some(count as i64 + 1),
            working_dir: root.to_owned(),
            repository_identity: IMPORT_DURABLE_IDENTITY_NOT_RETAINED.to_owned(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: commitment.clone(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"pipeline":"typed_allowlist","raw_persisted":false,"matches":[],"bytes_scanned":source.bytes().len(),"bytes_redacted":0}),
            transcript_snapshot: Some(CaptureSnapshotProjection {
                completeness: CaptureSnapshotCompleteness::Complete,
                partial_reason: None,
                source: Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: "not_retained:v1".to_owned(),
                    digest_sha256: Some(commitment),
                    byte_len: Some(source.bytes().len() as u64),
                }),
                transcript_redacted_bytes: source.bytes().len(),
                redaction_match_count: 0,
                redaction_bytes_scanned: source.bytes().len(),
                redaction_bytes_redacted: 0,
            }),
            turn_boundaries: turns
                .iter()
                .map(|turn| {
                    (
                        turn.logical_turn_key.clone(),
                        TurnBoundary {
                            started_at: turn.ordinal as i64 + 1,
                            ended_at: turn.ordinal as i64 + 2,
                        },
                    )
                })
                .collect(),
            turns,
        };
        (request, retained)
    }

    async fn summary_checkpoint_rows(
        conn: &DatabaseConnection,
    ) -> BTreeMap<String, (String, String, String)> {
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT checkpoint_id,tree_oid,metadata_blob_oid,traces_commit FROM agent_checkpoint"
                .to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get_by("checkpoint_id").unwrap(),
                (
                    row.try_get_by("tree_oid").unwrap(),
                    row.try_get_by("metadata_blob_oid").unwrap(),
                    row.try_get_by("traces_commit").unwrap(),
                ),
            )
        })
        .collect()
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_session_summary_upgrade_replays_old_dto_and_only_appends_new_turn() {
        use crate::internal::ai::capture::summary::SessionSummaryRead;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let storage = root.join(util::ROOT_DIR);
        std::fs::create_dir_all(storage.join("objects")).unwrap();
        let conn =
            crate::internal::db::create_database(storage.join(util::DATABASE).to_str().unwrap())
                .await
                .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        let scope = CaptureScope::resolve(&conn, &root).await.unwrap();
        let (old_request, old_retained) = summary_upgrade_request(&root, &scope, 2, false);
        summary_import(&conn, &storage, old_request, old_retained).await;
        let before = summary_checkpoint_rows(&conn).await;
        assert_eq!(before.len(), 2);
        let txn = conn.begin().await.unwrap();
        let view = SessionSummaryRead::prepare(&txn, &storage, "codex__cx07-upgrade")
            .await
            .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(view.finish().await.status, "unknown");
        let (mut request, retained) = summary_upgrade_request(&root, &scope, 2, true);
        let existing = load_existing_session_ownership_in_scope(
            &conn,
            AgentKind::Codex,
            "cx07-upgrade",
            &scope,
        )
        .await
        .unwrap();
        validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
        summary_import(&conn, &storage, request, retained).await;
        assert_eq!(
            summary_checkpoint_rows(&conn).await,
            before,
            "an upgraded replay cannot rewrite old metadata just to add a summary"
        );
        let txn = conn.begin().await.unwrap();
        let view = SessionSummaryRead::prepare(&txn, &storage, "codex__cx07-upgrade")
            .await
            .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(view.finish().await.status, "unknown");
        let (mut request, retained) = summary_upgrade_request(&root, &scope, 3, true);
        let existing = load_existing_session_ownership_in_scope(
            &conn,
            AgentKind::Codex,
            "cx07-upgrade",
            &scope,
        )
        .await
        .unwrap();
        validate_scoped_prepared_existing_session(&mut request, existing.as_ref()).unwrap();
        summary_import(&conn, &storage, request, retained).await;
        let after = summary_checkpoint_rows(&conn).await;
        assert_eq!(after.len(), 3);
        for (id, row) in before {
            assert_eq!(after.get(&id), Some(&row));
        }
        let txn = conn.begin().await.unwrap();
        let view = SessionSummaryRead::prepare(&txn, &storage, "codex__cx07-upgrade")
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let summary = view.finish().await;
        assert_eq!(summary.model.as_deref(), Some("CX07-UPGRADE"));
        assert_eq!(summary.prompt_count, Some(3));
        conn.close().await.unwrap();
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_summary_metadata_is_bounded_and_shared_report_keeps_all_turns() {
        let row = serde_json::json!({"turn_ordinal":0,"reasoning_tokens":9,
            "counts":{"readable_parts":1,"readable_utf8_bytes":12}});
        let report = serde_json::json!({"bytes_scanned":100,"codex_reasoning":{
            "schema_version":1,"source_status":"unproven","partial":true,
            "statistics_unavailable":false,"has_unproven_data":false,
            "turns":vec![row;4097]}});
        let metadata = checkpoint_redaction_metadata(AgentKind::Codex, &report);
        assert!(serde_json::to_vec(&metadata).unwrap().len() < 2048);
        assert!(metadata["codex_reasoning"].get("turns").is_none());
        assert_eq!(metadata["codex_reasoning"]["provider_turn_count"], 4097);
        assert_eq!(
            metadata["codex_reasoning"]["counts"]["readable_parts"],
            4097
        );
        assert_eq!(
            report["codex_reasoning"]["turns"].as_array().unwrap().len(),
            4097
        );
        assert_eq!(
            checkpoint_redaction_metadata(AgentKind::OpenCode, &report),
            report
        );
    }
}
