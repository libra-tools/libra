//! `libra agent import` — consented historical transcript backfill (M4).

#[cfg(any(unix, windows))]
use std::io::{Seek, SeekFrom, Write};
#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Args};
#[cfg(any(unix, windows))]
use ring::rand::SecureRandom;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Digest;

#[cfg(any(unix, windows))]
use crate::internal::ai::{
    agent_import::{
        LegacyImportMigrationRequest, import_provider_commitment, import_request_from_projection,
        import_source_preimage, import_storage_commitment,
        load_existing_session_ownership_in_scope, migrate_legacy_import_ownership_in_scope_until,
        validate_scoped_prepared_existing_session,
    },
    authorized_read::{CancellationSafeChild, read_async_strictly_bounded},
    hooks::runtime::{
        CaptureSourceCommitmentDomain, derive_capture_source_commitment_in_scope_until,
        derive_snapshot_content_commitment_in_scope_until,
    },
    observed_agents::{
        coverage::decode_redacted_import_runtime,
        reasoning::{ImportTransportKey, verify_import_transport},
    },
};
use crate::{
    internal::{
        ai::{
            agent_import::{
                DetailedImportSummary, IMPORT_INDEX_REPAIR_MARKER_KEY,
                IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2, ImportError, ImportIndexRepairMarker,
                ImportProgressError, ImportRequest, ImportSummary, PreparedImportProjection,
                identity_id as import_identity_id, import_prepared_with_retention,
                is_import_source_commitment_v2, prepare_import_projection_with_reasoning,
                restore_tombstone, session_is_tombstoned, validate_import_index_repair_marker,
            },
            authorized_read::{
                RegisteredHelperOutput, StrictBoundedRead, read_strictly_bounded,
                registered_helper_command, run_registered_bounded_helper_until,
            },
            capture::catalog::{CaptureCatalogStore, ReconciliationCatalogState},
            capture_scope::{
                CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
                authorize_final_capture_commit,
            },
            observed_agents::{
                AgentKind, AgentSessionCtx, CodexRolloutScanLimit, ExportAuthorized,
                TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource, agent_for, claude_session_dir,
                claude_session_id_is_safe_path_component,
                codex_index::{self, ImportIndexOutcome, ImportSourceState, RecentScopeStatus},
                compliance::{MAX_TRANSCRIPT_READ_BYTES_KEY, max_transcript_read_bytes_setting},
                coverage::{encode_bounded_json, import_runtime_has_typed_turns},
                find_codex_rollout_for_import,
                live_capture::{
                    LiveCaptureBinding, LiveCaptureContext, StoredSubagentSelection,
                    SubagentDiscoveryLimits,
                },
                open_provider_directory_for_discovery,
                opencode_export::{ExportLimits, authorized_trusted_sandboxed_export_until},
                reasoning::{
                    EncodedTransportKey, ImportTransportContext, ImportTransportDto,
                    NativeProvenReasoning, ReasoningProvider, seal_import_transport,
                },
                resolve_import_transcript_source_until, resolve_session_file,
            },
            provider_index_io,
        },
        db,
        metadata::{MetadataKv, MetadataScope, MetadataValueType},
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[cfg(all(test, feature = "test-live-agent"))]
tokio::task_local! {
    static INDEX_SOURCE_READS: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

// Counts every Pi session file the import opens for reading, so a test can
// prove that a refused consent reads no source (PI-10).
#[cfg(test)]
tokio::task_local! {
    static PI_SOURCE_READS: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

pub const AGENT_IMPORT_EXAMPLES: &str = "\
EXAMPLES:
    libra agent import --session <id> --agent claude-code --yes
    libra agent import --session <id> --agent codex --yes
    libra agent import --session <id> --agent opencode --yes
    libra agent import --path ~/.claude/projects/<project>/<id>.jsonl --agent claude-code --yes
    libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes
    libra agent import --all --agent claude-code --limit 20 --yes
    libra agent import --path <session-id>.jsonl --agent hermes --yes\n    libra agent import --path <timestamp>_<session-id>.jsonl --agent pi --yes\n    libra agent import --codex-index-backup ./closed-index.sqlite --limit 20 --yes";

const DEFAULT_IMPORT_LIMIT: usize = 20;
const MAX_IMPORT_LIMIT: usize = 100;
const MAX_BATCH_RAW_BYTES: u64 = 64 * 1024 * 1024;
const IMPORT_TOTAL_DEADLINE: Duration = Duration::from_secs(120);
pub const IMPORT_DISCOVERY_HELPER_ARG: &str = "--libra-internal-agent-import-discovery-helper";
/// Compact base64 path frames keep a full public 100-result page (including
/// near-`PATH_MAX` Unix paths) below this deliberately derived 2 MiB ceiling.
pub const IMPORT_DISCOVERY_HELPER_FRAME_CAP: u64 = 2 * 1024 * 1024;
pub const IMPORT_PREPARATION_HELPER_OUTPUT_CAP: u64 = 128 * 1024 * 1024;
/// Descriptor-capability preparation protocol. Its stdin is the already
/// authorized transcript descriptor (or an anonymous bounded export file),
/// never a JSON envelope containing a provider locator or session id.
pub const IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG: &str =
    "--libra-internal-agent-import-preparation-descriptor-helper";
pub const IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV: &str =
    "LIBRA_INTERNAL_IMPORT_PREPARATION_DESCRIPTOR_CONTROL";
pub const IMPORT_INDEX_REPAIR_HELPER_ARG: &str =
    "--libra-internal-agent-import-index-repair-helper";
pub const IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP: u64 = 64 * 1024;
/// Longer than the command's absolute deadline so a healthy importer cannot
/// be preempted while it is finishing a final SQLite commit or index drain.
/// Normal failures explicitly release the lease into `repair_pending`, so
/// only a process crash requires waiting for this TTL.
const IMPORT_INDEX_BARRIER_LEASE_MS: i64 = 180_000;

// Test controls deliberately live behind `cfg(test)`: `cargo build` uses a
// debug profile by default, so `cfg!(debug_assertions)` is not a safe boundary
// for fault injection or filesystem rendezvous hooks.
#[cfg(test)]
mod test_support {
    use std::sync::{Mutex, OnceLock, mpsc};

    use super::*;

    #[derive(Default)]
    pub(super) struct ImportTestControls {
        pub(super) total_deadline: Option<Duration>,
        pub(super) batch_raw_byte_cap: Option<u64>,
        pub(super) codex_home: Option<PathBuf>,
        pub(super) fail_index_tombstone_lookup: bool,
        pub(super) preparation_response_read_delay: Option<Duration>,
        pub(super) source_open_pause: Option<TestPause>,
        pub(super) index_barrier_pause: Option<TestPause>,
        /// Test-only deterministic window between persisting a barrier value
        /// and its final deadline/fence checks. Production never sleeps here.
        pub(super) index_barrier_persist_delay: Option<Duration>,
    }

    pub(super) struct TestPause {
        pub(super) reached: mpsc::Sender<()>,
        pub(super) resume: mpsc::Receiver<()>,
    }

    impl TestPause {
        fn wait(self, deadline: Instant, stage: &'static str) -> Result<()> {
            let _ = self.reached.send(());
            self.resume
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| anyhow::anyhow!("test {stage} pause exceeded the import deadline"))
        }
    }

    static CONTROLS: OnceLock<Mutex<ImportTestControls>> = OnceLock::new();

    fn controls() -> &'static Mutex<ImportTestControls> {
        CONTROLS.get_or_init(|| Mutex::new(ImportTestControls::default()))
    }

    fn lock_controls() -> std::sync::MutexGuard<'static, ImportTestControls> {
        controls()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) struct ControlsReset(Option<ImportTestControls>);

    pub(super) fn install(controls: ImportTestControls) -> ControlsReset {
        let previous = std::mem::replace(&mut *lock_controls(), controls);
        ControlsReset(Some(previous))
    }

    impl Drop for ControlsReset {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = std::mem::replace(&mut *lock_controls(), previous);
            }
        }
    }

    pub(super) fn total_deadline() -> Option<Duration> {
        lock_controls().total_deadline
    }

    pub(super) fn batch_raw_byte_cap() -> Option<u64> {
        lock_controls().batch_raw_byte_cap
    }

    pub(super) fn codex_home() -> Option<PathBuf> {
        lock_controls().codex_home.clone()
    }

    pub(super) fn fail_index_tombstone_lookup() -> bool {
        lock_controls().fail_index_tombstone_lookup
    }

    pub(super) fn preparation_response_read_delay() -> Option<Duration> {
        lock_controls().preparation_response_read_delay
    }

    pub(super) fn take_source_open_pause() -> Option<TestPause> {
        lock_controls().source_open_pause.take()
    }

    pub(super) fn take_index_barrier_pause() -> Option<TestPause> {
        lock_controls().index_barrier_pause.take()
    }

    pub(super) fn wait_source_open_pause(deadline: Instant) -> Result<()> {
        match take_source_open_pause() {
            Some(pause) => pause.wait(deadline, "source-open"),
            None => Ok(()),
        }
    }

    pub(super) fn wait_index_barrier_pause(deadline: Instant) -> Result<()> {
        match take_index_barrier_pause() {
            Some(pause) => pause.wait(deadline, "index-barrier"),
            None => Ok(()),
        }
    }

    pub(super) fn index_barrier_persist_delay() -> Option<Duration> {
        lock_controls().index_barrier_persist_delay
    }
}

fn import_total_deadline() -> Duration {
    #[cfg(test)]
    if let Some(deadline) = test_support::total_deadline()
        && !deadline.is_zero()
    {
        return deadline.min(IMPORT_TOTAL_DEADLINE);
    }
    IMPORT_TOTAL_DEADLINE
}

fn ensure_before_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(ImportError::DeadlineExceeded.into());
    }
    Ok(())
}

/// Bound only a read-only import preflight. A dispatched mutation, final
/// authorization, and COMMIT acknowledgement must remain outside this helper:
/// cancelling any of those futures can leave the durable outcome unknowable.
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

fn import_commitment_failure(error: anyhow::Error, deadline: Instant) -> ImportError {
    let authorization_deadline = error
        .downcast_ref::<CaptureFinalCommitAuthorizationError>()
        .is_some_and(|error| {
            matches!(
                error,
                &CaptureFinalCommitAuthorizationError::DeadlineElapsed
            )
        });
    if Instant::now() >= deadline || authorization_deadline {
        ImportError::DeadlineExceeded
    } else {
        ImportError::AuthorizedReaderFailed
    }
}

#[cfg(test)]
fn import_test_pause_after_source_open(deadline: Instant) -> Result<()> {
    test_support::wait_source_open_pause(deadline)
}

#[cfg(not(test))]
fn import_test_pause_after_source_open(_deadline: Instant) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn import_test_pause_after_index_barrier(deadline: Instant) -> Result<()> {
    test_support::wait_index_barrier_pause(deadline)
}

#[cfg(not(test))]
fn import_test_pause_after_index_barrier(_deadline: Instant) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn import_test_delay_after_index_barrier_persist() {
    if let Some(delay) = test_support::index_barrier_persist_delay() {
        // Keep this synchronous in the test seam. The production timeout
        // still bounds SQLite waits, while this deterministic post-DML pause
        // proves that the explicit pre-commit deadline gate rolls the
        // transaction back rather than relying on task cancellation.
        std::thread::sleep(delay);
    }
}

#[cfg(not(test))]
fn import_test_delay_after_index_barrier_persist() {}

fn batch_raw_byte_cap() -> u64 {
    #[cfg(test)]
    if let Some(cap) = test_support::batch_raw_byte_cap()
        && cap > 0
    {
        return cap.min(MAX_BATCH_RAW_BYTES);
    }
    MAX_BATCH_RAW_BYTES
}

fn effective_source_read_cap(configured_cap: u64) -> u64 {
    configured_cap.min(TRANSCRIPT_READ_HARD_CAP_BYTES)
}

fn remaining_candidate_read_allowance(source_read_cap: u64, parent_bytes: u64) -> u64 {
    source_read_cap.saturating_sub(parent_bytes)
}

#[derive(Args, Debug)]
#[command(
    after_help = AGENT_IMPORT_EXAMPLES,
    group(ArgGroup::new("selector").required(true).multiple(false).args(["session", "path", "since", "all", "codex_index_backup"]))
)]
pub struct ImportArgs {
    /// Import one provider session id.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Import one transcript below the selected provider root.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Discover sessions since this RFC3339 timestamp (Codex uses session_meta time).
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    /// Discover all bounded local Claude/Codex session sources.
    #[arg(long)]
    pub all: bool,
    /// Backfill absent Codex sessions from an explicit closed full-history index backup.
    #[arg(long, value_name = "PATH")]
    pub codex_index_backup: Option<PathBuf>,
    /// Provider filter (`claude-code`, `codex`, `opencode`, `hermes`, or `pi`). `pi` imports only one `--path` session file, locally (no scan, no upload), and stores its snapshot as partial and unproven.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// Maximum discovered sessions to process (default 20, maximum 100).
    #[arg(long, value_name = "N", default_value_t = DEFAULT_IMPORT_LIMIT)]
    pub limit: usize,
    /// Zero-based opaque-enough discovery cursor returned by a prior page.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<usize>,
    /// Confirm reading/redacting provider session data into this repository.
    #[arg(long)]
    pub yes: bool,
    /// Explicitly remove an existing local anti-resurrection tombstone.
    #[arg(long, requires = "yes", conflicts_with = "codex_index_backup")]
    pub restore_erased: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    kind: AgentKind,
    provider_session_id: String,
    path: Option<PathBuf>,
}

type DiscoveryBatch = (
    Vec<Candidate>,
    Option<usize>,
    Vec<(String, DiscoveryRejection)>,
);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WirePath {
    #[cfg(unix)]
    #[serde(with = "wire_path_bytes_serde")]
    bytes: Vec<u8>,
    #[cfg(not(unix))]
    text: String,
}

#[cfg(unix)]
mod wire_path_bytes_serde {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded)
            .map_err(|error| D::Error::custom(format!("invalid base64 path frame: {error}")))
    }
}

impl WirePath {
    fn from_path(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            Self {
                bytes: path.as_os_str().as_bytes().to_vec(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                text: path.to_string_lossy().into_owned(),
            }
        }
    }

    fn into_path_buf(self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::{ffi::OsString, os::unix::ffi::OsStringExt};

            PathBuf::from(OsString::from_vec(self.bytes))
        }
        #[cfg(not(unix))]
        {
            PathBuf::from(self.text)
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveryHelperRequest {
    repo_root: WirePath,
    session: Option<String>,
    path: Option<WirePath>,
    since: Option<String>,
    all: bool,
    agent: Option<String>,
    limit: usize,
    cursor: Option<usize>,
    remaining_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveryCandidateWire {
    kind: String,
    provider_session_id: String,
    path: Option<WirePath>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum DiscoveryHelperResponse {
    Ok {
        candidates: Vec<DiscoveryCandidateWire>,
        next_cursor: Option<usize>,
        rejected: Vec<(String, DiscoveryRejection)>,
    },
    Error {
        reason: DiscoveryRejection,
    },
}

/// Closed reason a discovery helper reports to its parent. Only this fixed
/// enum crosses the process boundary; the parent renders the matching
/// actionable message, so no provider path or error chain is reflected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscoveryRejection {
    /// The helper rejected argv the parent already validated; reachable only
    /// if the two copies disagree, e.g. after a working-directory change.
    SelectorRejected,
    AmbiguousSession,
    SessionNotFound,
    CursorOutOfRange,
    ClaudeProviderRoot,
    CodexProviderRoot,
    CodexUnknownSessionTime,
    CodexSourceUnreadable,
    CodexDiscoveryBudgetExceeded,
    DeadlineExceeded,
}

#[derive(Debug, thiserror::Error)]
#[error("Codex session_meta has no timestamp")]
struct CodexDiscoveryUnknownTime;

#[derive(Debug, thiserror::Error)]
#[error("Codex cumulative discovery header budget exceeded")]
struct CodexDiscoveryBudgetExceeded;

#[derive(Debug, thiserror::Error)]
#[error("Codex discovery entry budget exceeded")]
struct CodexDiscoveryEntryBudgetExceeded;

impl DiscoveryRejection {
    /// Classify a provider discovery failure without retaining its chain.
    fn from_provider_error(error: &anyhow::Error, fallback: Self) -> Self {
        if matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ) {
            Self::DeadlineExceeded
        } else if error.downcast_ref::<CodexDiscoveryUnknownTime>().is_some() {
            Self::CodexUnknownSessionTime
        } else if error
            .downcast_ref::<CodexDiscoveryBudgetExceeded>()
            .is_some()
            || error
                .downcast_ref::<CodexDiscoveryEntryBudgetExceeded>()
                .is_some()
            || error.downcast_ref::<CodexRolloutScanLimit>().is_some()
        {
            Self::CodexDiscoveryBudgetExceeded
        } else {
            fallback
        }
    }

    fn into_cli_error(self) -> CliError {
        match self {
            Self::SelectorRejected => {
                CliError::command_usage("agent import discovery rejected the supplied selector")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            }
            Self::AmbiguousSession => {
                CliError::command_usage("the session id matches multiple providers; add --agent")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            }
            Self::SessionNotFound => CliError::fatal(
                "no authorized local transcript matched the session id; use --agent opencode for an export-only OpenCode session",
            )
            .with_stable_code(StableErrorCode::CliInvalidTarget),
            Self::CursorOutOfRange => {
                CliError::command_usage("--cursor is outside the discovery result set")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            }
            Self::ClaudeProviderRoot => CliError::fatal(
                "Claude session discovery failed within its configured provider root",
            )
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing),
            Self::CodexProviderRoot => CliError::fatal(
                "Codex session discovery failed within its configured provider root",
            )
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing),
            Self::CodexUnknownSessionTime => CliError::fatal(
                "Codex session_meta has no timestamp; --since cannot use filesystem mtime. Inspect the rollout and import it with --session or --path",
            )
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing),
            Self::CodexSourceUnreadable => CliError::fatal(
                "Codex rollout header could not be read; inspect source permissions and retry with --session or --path",
            )
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing),
            Self::CodexDiscoveryBudgetExceeded => CliError::fatal(
                "Codex discovery exceeded its 256 MiB header, 4096 unfilterable-record, or directory entry budget; import a specific rollout with --path, or use a closed Codex index backup for paged history",
            )
            .with_stable_code(StableErrorCode::AgentImportPartialBatch),
            Self::DeadlineExceeded => {
                CliError::fatal("agent import discovery exceeded its total execution deadline")
                    .with_stable_code(StableErrorCode::AgentImportPartialBatch)
            }
        }
    }
}

/// Safe control for the descriptor-owning helper. Every field is either a
/// fixed enum/bound or a fixed-size commitment; raw provider IDs, source
/// locators, repository paths, and transcript bytes are deliberately absent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationDescriptorControl {
    agent_kind: String,
    source_kind: String,
    provider_commitment: [u8; 32],
    read_cap: u64,
    remaining_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<PreparationImportTransportControl>,
}

/// The only new request wire frame. The source and scope values are opaque
/// commitments, never source identifiers or repository paths. The encoded
/// key has a redacted Debug implementation in the lower reasoning module.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationImportTransportControl {
    version: u8,
    challenge: [u8; 32],
    source_preimage_commitment: [u8; 32],
    scope_digest: [u8; 32],
    key: EncodedTransportKey,
}

impl std::fmt::Debug for PreparationImportTransportControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparationImportTransportControl")
            .field("version", &self.version)
            .field("challenge", &"[redacted]")
            .field("source_preimage_commitment", &"[redacted]")
            .field("scope_digest", &"[redacted]")
            .field("key", &"[redacted]")
            .finish()
    }
}

/// Expected request bindings retained only in the parent process after the
/// control frame has moved into the descriptor helper. None is a secret key.
struct PreparationImportTransportBinding {
    challenge: [u8; 32],
    request_digest: [u8; 32],
    source_preimage_commitment: [u8; 32],
    scope_digest: [u8; 32],
}

fn import_transport_digest(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(domain);
    for field in fields {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}

#[cfg(any(unix, windows))]
fn import_transport_source_preimage_commitment(preimage: &[u8; 32]) -> [u8; 32] {
    import_transport_digest(b"libra/import-transport-source-preimage/v1\0", &[preimage])
}

#[cfg(any(unix, windows))]
fn import_transport_scope_digest(scope: &CaptureScope) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(scope).map_err(|_| ImportError::AuthorizedReaderFailed)?;
    Ok(import_transport_digest(
        b"libra/import-transport-scope/v1\0",
        &[&encoded],
    ))
}

impl PreparationImportTransportBinding {
    fn from_control(control: &PreparationDescriptorControl) -> Result<Option<Self>> {
        let Some(transport) = control.transport.as_ref() else {
            return Ok(None);
        };
        if transport.version != 1 {
            return Err(ImportError::AuthorizedReaderFailed.into());
        }
        let request_digest = import_transport_digest(
            b"libra/import-transport-request/v1\0",
            &[
                control.agent_kind.as_bytes(),
                control.source_kind.as_bytes(),
                &control.provider_commitment,
                &control.read_cap.to_le_bytes(),
                &control.remaining_ms.to_le_bytes(),
                &[transport.version],
                &transport.challenge,
                &transport.source_preimage_commitment,
                &transport.scope_digest,
            ],
        );
        Ok(Some(Self {
            challenge: transport.challenge,
            request_digest,
            source_preimage_commitment: transport.source_preimage_commitment,
            scope_digest: transport.scope_digest,
        }))
    }

    fn context(
        &self,
        agent_kind: AgentKind,
        source_session: &str,
        projection: &PreparedImportProjection,
    ) -> Result<ImportTransportContext> {
        let snapshot_preimage = projection
            .transcript_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.redacted_digest_preimage())
            .ok_or(ImportError::AuthorizedReaderFailed)?;
        let source_digest = import_transport_digest(
            b"libra/import-transport-authorized-source/v1\0",
            &[&self.source_preimage_commitment, &snapshot_preimage],
        );
        let encoded_projection =
            serde_json::to_vec(projection).map_err(|_| ImportError::AuthorizedReaderFailed)?;
        let projection_digest = import_transport_digest(
            b"libra/import-transport-projection/v1\0",
            &[&encoded_projection],
        );
        let provider = match agent_kind {
            AgentKind::ClaudeCode => ReasoningProvider::ClaudeCode,
            AgentKind::Codex => ReasoningProvider::Codex,
            AgentKind::OpenCode => ReasoningProvider::OpenCode,
            _ => return Err(ImportError::AuthorizedReaderFailed.into()),
        };
        let capture_session = crate::internal::ai::hooks::runtime::build_ai_session_id(
            provider_name(agent_kind),
            source_session,
        );
        ImportTransportContext::new(
            self.challenge,
            self.request_digest,
            source_digest,
            self.scope_digest,
            projection_digest,
            provider,
            capture_session,
            source_session.to_owned(),
        )
        .map_err(|_| ImportError::AuthorizedReaderFailed.into())
    }
}

/// The single production parent consumer for a helper's untrusted optional
/// typed response. The caller must invoke it before any scoped key derivation
/// or legacy ownership migration.
#[cfg(any(unix, windows))]
fn consume_prepared_reasoning_transport(
    key: ImportTransportKey,
    binding: &PreparationImportTransportBinding,
    agent_kind: AgentKind,
    source_session: &str,
    projection: &PreparedImportProjection,
    transport_required: Option<bool>,
    transport: Option<ImportTransportDto>,
) -> Result<Option<NativeProvenReasoning>> {
    let transport = match (transport_required, transport) {
        (None, None) => return Ok(None),
        (Some(true), Some(transport)) => transport,
        _ => return Err(ImportError::AuthorizedReaderFailed.into()),
    };
    let context = binding.context(agent_kind, source_session, projection)?;
    verify_import_transport(key, context, transport)
        .map(Some)
        .context(ImportError::AuthorizedReaderFailed)
}

/// Shape validation only: source/native authentication remains the caller's
/// prerequisite. Missing runtime data cannot choose a legacy OpenCode path.
#[cfg_attr(windows, allow(dead_code))]
fn validate_prepared_runtime_response(
    kind: AgentKind,
    projection: &PreparedImportProjection,
    native_present: bool,
) -> Result<()> {
    if projection.codex_retention.is_some() && (kind != AgentKind::Codex || native_present) {
        return Err(ImportError::AuthorizedReaderFailed.into());
    }
    if kind == AgentKind::Codex && projection.codex_retention.is_none() {
        return Err(ImportError::AuthorizedReaderFailed.into());
    }
    match (kind, projection.coverage_runtime.as_ref()) {
        (AgentKind::OpenCode, Some(dto))
            if !native_present || !import_runtime_has_typed_turns(dto) =>
        {
            Ok(())
        }
        (AgentKind::OpenCode, _) | (_, Some(_)) => Err(ImportError::AuthorizedReaderFailed.into()),
        (_, None) => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreparationImportErrorKind {
    WorkingDirMissingOrAmbiguous,
    RepositoryConflict,
    SessionIdentityConflict,
    Erased,
    LeaseBusy,
    SourceAuthorization,
    NoImportableTurns,
    BatchInputLimit,
    DeadlineExceeded,
    FutureTimestamp,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum PreparationDescriptorHelperResponse {
    Ok {
        projection: Box<PreparedImportProjection>,
        raw_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transport_required: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transport: Option<ImportTransportDto>,
    },
    Error {
        error_kind: Option<PreparationImportErrorKind>,
        raw_bytes: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexRepairHelperRequest {
    storage_root: WirePath,
    session_id: String,
    marker_owner: String,
    marker_generation: String,
    agent_kind: String,
    provider_session_id: String,
    capture_scope: CaptureScope,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum IndexRepairHelperResponse {
    Ok { repaired_rows: usize },
    Error {},
}

#[derive(Debug, Clone)]
struct ImportIndexBarrier {
    session_id: String,
    marker: ImportIndexRepairMarker,
    capture_scope: CaptureScope,
}

#[derive(Debug, Clone)]
struct ImportIdentityFence {
    identity_id: String,
    fence_token: i64,
}

struct PreparedCandidate {
    request: ImportRequest,
    native_reasoning: Option<NativeProvenReasoning>,
    coverage_runtime: Option<crate::internal::ai::capture::snapshot::CaptureCoverageRuntime>,
    codex_retention: Option<crate::internal::ai::observed_agents::codex_reasoning::CodexRetention>,
}

struct PreparedCandidateOutcome {
    request: Result<PreparedCandidate>,
    raw_bytes: u64,
}

#[derive(Debug, Serialize)]
struct BatchOutput {
    schema_version: u32,
    /// Fully completed selections only. Partial durable progress is reported
    /// separately so automation never counts a failed selection as success.
    results: Vec<BatchResult>,
    partial_results: Vec<BatchResult>,
    skipped: Vec<BatchSkip>,
    failures: Vec<BatchFailure>,
    next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    existing: Vec<BatchExisting>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BatchExisting {
    status: &'static str,
    agent_kind: String,
    session_id: String,
    selection_reason: &'static str,
}

#[derive(Debug, Serialize)]
struct BatchResult {
    status: &'static str,
    #[serde(flatten)]
    summary: ImportSummary,
    subagent_checkpoints_written: usize,
}

impl BatchResult {
    fn complete(detailed: DetailedImportSummary) -> Self {
        let status = if detailed.summary.checkpoints_written == 0
            && detailed.subagent_checkpoints_written == 0
        {
            "noop"
        } else {
            "imported"
        };
        Self {
            status,
            summary: detailed.summary,
            subagent_checkpoints_written: detailed.subagent_checkpoints_written,
        }
    }

    fn partial(detailed: DetailedImportSummary) -> Self {
        Self {
            status: "partial",
            summary: detailed.summary,
            subagent_checkpoints_written: detailed.subagent_checkpoints_written,
        }
    }
}

#[derive(Debug, Serialize)]
struct BatchSkip {
    status: &'static str,
    agent_kind: String,
    session_id: String,
    reason_code: StableErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_key: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct BatchFailure {
    status: &'static str,
    agent_kind: String,
    session_id: String,
    error_code: StableErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_key: Option<&'static str>,
}

/// The CLI accepts exactly the prepared-import kind set, so the two can never
/// disagree (PI-29).
fn importable_kind_from_slug(slug: &str) -> Option<AgentKind> {
    AgentKind::from_cli_slug(slug)
        .filter(|kind| crate::internal::ai::agent_import::is_importable_kind(*kind))
}

/// Parse the user's own `--agent` argv. The echo is bounded and has control
/// characters replaced so an oversized value cannot flood or drive a terminal.
fn importable_kind(slug: &str) -> CliResult<AgentKind> {
    importable_kind_from_slug(slug).ok_or_else(|| {
        CliError::command_usage(format!(
            "agent import supports claude-code, codex, opencode, hermes, or pi; got '{}'",
            bounded_argv_echo(slug)
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
    })
}

fn bounded_argv_echo(value: &str) -> String {
    const MAX_ECHO_CHARS: usize = 64;
    let mut echo = value
        .chars()
        .take(MAX_ECHO_CHARS)
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect::<String>();
    if value.chars().nth(MAX_ECHO_CHARS).is_some() {
        echo.push('…');
    }
    echo
}

fn provider_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        other => other.as_db_str(),
    }
}

fn reserve_subagent_input_allowance(cumulative: &mut u64, allowance: u64) -> Result<()> {
    *cumulative = cumulative
        .checked_add(allowance)
        .ok_or(ImportError::BatchInputLimit)?;
    Ok(())
}

fn settle_subagent_input_allowance(
    cumulative: &mut u64,
    reserved_allowance: u64,
    bytes_read: u64,
) -> Result<()> {
    if bytes_read > reserved_allowance {
        return Err(ImportError::BatchInputLimit.into());
    }
    *cumulative = cumulative
        .saturating_sub(reserved_allowance)
        .checked_add(bytes_read)
        .ok_or(ImportError::BatchInputLimit)?;
    Ok(())
}

fn codex_session_id_is_safe_path_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn session_id_is_valid_for_kind(value: &str, kind: AgentKind) -> bool {
    match kind {
        AgentKind::Codex | AgentKind::OpenCode | AgentKind::Hermes => {
            codex_session_id_is_safe_path_component(value)
        }
        AgentKind::ClaudeCode => claude_session_id_is_safe_path_component(value),
        AgentKind::Pi => pi_session_id_is_valid(value),
        _ => false,
    }
}

/// Pi's session id grammar, `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`
/// (`assertValidSessionId`, `session-manager.ts:268-274`), bounded to 128
/// characters here. The header `id` the import reads must equal it.
fn pi_session_id_is_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    bytes.len() <= 128
        && first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_session_id_for_kind(value: &str, kind: AgentKind) -> CliResult<()> {
    if session_id_is_valid_for_kind(value, kind) {
        Ok(())
    } else {
        let expected = match kind {
            AgentKind::Codex | AgentKind::OpenCode | AgentKind::Hermes => {
                "alphanumeric/dash/underscore, at most 64 characters"
            }
            AgentKind::ClaudeCode => "alphanumeric/dot/dash/underscore, at most 128 characters",
            AgentKind::Pi => {
                "alphanumeric/dot/dash/underscore, starting and ending alphanumeric, at most 128 characters"
            }
            _ => "a safe provider session path component",
        };
        Err(CliError::command_usage(format!(
            "invalid {} session id (expected {expected})",
            provider_name(kind)
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments))
    }
}

fn codex_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    let suffix = stem.get(stem.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(suffix).ok().map(|id| id.to_string())
}

fn hermes_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    session_id_is_valid_for_kind(stem, AgentKind::Hermes).then(|| stem.to_string())
}

/// Pi names a session file `<timestamp>_<id>.jsonl`
/// (`session-manager.ts:1079-1080`); the timestamp has no underscore, so the
/// id is everything after the first one. The header is the record: the
/// import later requires the header `id` to equal this id.
fn pi_session_id_from_filename(path: &Path) -> Option<String> {
    if path.extension()? != "jsonl" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (timestamp, id) = stem.split_once('_')?;
    (pi_file_timestamp_is_valid(timestamp) && pi_session_id_is_valid(id)).then(|| id.to_string())
}

/// The timestamp Pi puts before the id: `new Date().toISOString()` with `:`
/// and `.` replaced by `-`, as `2026-10-31T00-00-00-000Z`.
fn pi_file_timestamp_is_valid(timestamp: &str) -> bool {
    const SHAPE: &[u8; 24] = b"dddd-dd-ddTdd-dd-dd-dddZ";
    let bytes = timestamp.as_bytes();
    bytes.len() == SHAPE.len()
        && bytes.iter().zip(SHAPE).all(|(byte, shape)| match shape {
            b'd' => byte.is_ascii_digit(),
            other => byte == other,
        })
}

fn claude_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    validate_session_id_for_kind(&stem, AgentKind::ClaudeCode)
        .ok()
        .map(|()| stem)
}

fn path_candidate(path: PathBuf, kind: AgentKind) -> CliResult<Candidate> {
    if kind == AgentKind::Pi
        && path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        // The Pi file is opened without following a symlink at any
        // directory, so a `..` step, which may cross one, is refused up front.
        return Err(CliError::command_usage(
            "Pi import needs a --path without `..` components; pass the session file's absolute path or a path below the current directory",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if kind == AgentKind::OpenCode {
        return Err(CliError::command_usage(
            "opencode has no transcript file; use --session so Libra can run the trusted export bridge",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| {
                CliError::fatal(format!("failed to resolve current directory: {error}"))
            })?
            .join(path)
    };
    let provider_session_id = match kind {
        AgentKind::ClaudeCode => claude_session_id_from_filename(&absolute),
        AgentKind::Codex => codex_session_id_from_filename(&absolute),
        AgentKind::Hermes => hermes_session_id_from_filename(&absolute),
        AgentKind::Pi => pi_session_id_from_filename(&absolute),
        _ => None,
    }
    .ok_or_else(|| {
        CliError::command_usage(
            "the selected transcript filename does not contain a valid provider session id",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    Ok(Candidate {
        kind,
        provider_session_id,
        path: Some(absolute),
    })
}

fn metadata_modified_at(metadata: &std::fs::Metadata) -> Result<i64> {
    let modified = metadata
        .modified()
        .context("read provider source modification time")?;
    Ok(modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiscoveryEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[cfg(unix)]
fn discovery_entry_kind_at(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<DiscoveryEntryKind> {
    use std::{
        ffi::CString,
        mem::MaybeUninit,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    let name = CString::new(name.as_bytes()).context("provider entry name contains NUL")?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the parent fd is live, name is NUL-terminated, and `stat`
    // points to writable storage. AT_SYMLINK_NOFOLLOW prevents discovery
    // from dereferencing an untrusted entry before consent.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(DiscoveryEntryKind::Other);
        }
        return Err(error).context("inspect provider discovery entry");
    }
    // SAFETY: fstatat succeeded and initialized the structure.
    let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
    Ok(match mode {
        libc::S_IFDIR => DiscoveryEntryKind::Directory,
        libc::S_IFREG => DiscoveryEntryKind::File,
        libc::S_IFLNK => DiscoveryEntryKind::Symlink,
        _ => DiscoveryEntryKind::Other,
    })
}

#[cfg(unix)]
fn open_discovery_entry_at(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    expected: DiscoveryEntryKind,
) -> Result<Option<std::fs::File>> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let observed = discovery_entry_kind_at(directory, name)?;
    if observed == DiscoveryEntryKind::Symlink {
        anyhow::bail!("provider discovery encountered a symlinked entry (fail-closed)");
    }
    if observed != expected {
        return Ok(None);
    }
    let name = CString::new(name.as_bytes()).context("provider entry name contains NUL")?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if expected == DiscoveryEntryKind::Directory {
        flags |= libc::O_DIRECTORY;
    } else {
        flags |= libc::O_NONBLOCK;
    }
    // SAFETY: the parent fd is live, name is NUL-terminated, and a successful
    // descriptor is immediately transferred to File.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error).context("open provider discovery entry without following links");
    }
    // SAFETY: fd is freshly returned by openat and transferred once.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .context("inspect opened provider discovery entry")?;
    let still_expected = match expected {
        DiscoveryEntryKind::Directory => metadata.is_dir(),
        DiscoveryEntryKind::File => metadata.is_file(),
        DiscoveryEntryKind::Symlink | DiscoveryEntryKind::Other => false,
    };
    if !still_expected {
        anyhow::bail!("provider discovery entry changed type while being opened");
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn read_discovery_names(
    directory: &std::fs::File,
    label: &str,
    deadline: Instant,
    scanned: &mut usize,
) -> Result<Vec<std::ffi::OsString>> {
    ensure_before_deadline(deadline)?;
    let read = crate::internal::ai::observed_agents::read_dir_pinned_provider_directory(directory)
        .with_context(|| format!("read pinned {label} directory"))?;
    let mut names = Vec::new();
    for entry in read {
        ensure_before_deadline(deadline)?;
        *scanned = scanned
            .checked_add(1)
            .context("provider discovery entry counter overflow")?;
        if *scanned > 20_000 {
            return Err(CodexDiscoveryEntryBudgetExceeded.into());
        }
        names.push(
            entry
                .context("read pinned provider directory entry")?
                .file_name,
        );
    }
    names.sort_unstable_by(|left, right| right.cmp(left));
    ensure_before_deadline(deadline)?;
    Ok(names)
}

fn discover_claude(
    repo_root: &Path,
    since: Option<i64>,
    deadline: Instant,
) -> Result<Vec<Candidate>> {
    ensure_before_deadline(deadline)?;
    let Some(dir) = claude_session_dir(repo_root) else {
        return Ok(Vec::new());
    };
    let adapter = agent_for(AgentKind::ClaudeCode);
    let Some(directory) = open_provider_directory_for_discovery(adapter, &dir)? else {
        return Ok(Vec::new());
    };
    #[cfg(unix)]
    let entries =
        crate::internal::ai::observed_agents::read_dir_pinned_provider_directory(&directory)
            .context("read pinned Claude session directory")?;
    #[cfg(not(unix))]
    let entries = {
        let _ = &directory;
        std::fs::read_dir(&dir).context("read pinned Claude session directory")?
    };
    let mut candidates = Vec::new();
    for (index, entry) in entries.enumerate() {
        ensure_before_deadline(deadline)?;
        if index >= 20_000 {
            anyhow::bail!("Claude discovery exceeded its 20000-entry safety bound");
        }
        let entry = entry.context("read Claude session directory entry")?;
        #[cfg(unix)]
        let file_name = entry.file_name;
        #[cfg(not(unix))]
        let file_name = entry.file_name();
        #[cfg(unix)]
        let Some(opened) =
            open_discovery_entry_at(&directory, &file_name, DiscoveryEntryKind::File)?
        else {
            continue;
        };
        #[cfg(not(unix))]
        let opened = {
            let file_type = entry
                .file_type()
                .context("inspect Claude session source type")?;
            if file_type.is_symlink() {
                anyhow::bail!(
                    "Claude discovery encountered a symlinked session source (fail-closed)"
                );
            }
            if !file_type.is_file() {
                continue;
            }
            entry
                .metadata()
                .context("inspect pinned Claude session source")?
        };
        let path = dir.join(&file_name);
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        #[cfg(unix)]
        let metadata = opened
            .metadata()
            .context("inspect opened Claude session source")?;
        #[cfg(not(unix))]
        let metadata = opened;
        if let Some(boundary) = since
            && metadata_modified_at(&metadata)? < boundary
        {
            continue;
        }
        let Some(provider_session_id) = claude_session_id_from_filename(&path) else {
            continue;
        };
        candidates.push(Candidate {
            kind: AgentKind::ClaudeCode,
            provider_session_id,
            path: Some(path),
        });
    }
    Ok(candidates)
}

fn codex_sessions_root() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").map(PathBuf::from)
        && home.is_absolute()
    {
        return Some(home.join("sessions"));
    }
    #[cfg(test)]
    if let Some(home) = test_support::codex_home() {
        return Some(home.join(".codex").join("sessions"));
    }
    dirs::home_dir().map(|home| home.join(".codex").join("sessions"))
}

const CODEX_DISCOVERY_HEADER_CAP: u64 = 1024 * 1024;
const CODEX_DISCOVERY_HEADER_TOTAL_CAP: u64 = 256 * 1024 * 1024;
const CODEX_DISCOVERY_REJECTION_CAP: usize = 4096;

/// Read only the bounded first record from an already authorized, pinned
/// rollout descriptor. Its session timestamp, rather than filesystem mtime,
/// decides `--since`; a missing timestamp cannot be guessed from an archive
/// move or a copied file.
fn codex_discovery_session_time(
    file: std::fs::File,
    expected_session_id: &str,
    header_bytes: &mut u64,
) -> Result<Option<i64>> {
    use std::io::{BufRead, Read};

    let mut reader = std::io::BufReader::new(file.take(CODEX_DISCOVERY_HEADER_CAP + 1));
    let mut first = Vec::new();
    let read_result = reader.read_until(b'\n', &mut first);
    // BufReader can prefetch beyond the first newline. Charge every byte
    // consumed from the held descriptor, including prefetch and failed reads.
    let consumed = CODEX_DISCOVERY_HEADER_CAP + 1 - reader.get_ref().limit();
    *header_bytes = header_bytes
        .checked_add(consumed)
        .context("Codex discovery header byte counter overflow")?;
    if *header_bytes > CODEX_DISCOVERY_HEADER_TOTAL_CAP {
        return Err(CodexDiscoveryBudgetExceeded.into());
    }
    read_result.context("read bounded Codex session header")?;
    if first.len() as u64 > CODEX_DISCOVERY_HEADER_CAP {
        anyhow::bail!("Codex session header exceeds the 1 MiB discovery bound");
    }
    let header = crate::internal::ai::observed_agents::parse_canon_value(&first)
        .context("decode Codex session header with unique JSON keys")?;
    if header.get("type").and_then(|value| value.as_str()) != Some("session_meta") {
        anyhow::bail!(
            "Codex rollout has no first-record session_meta; use an explicit source after inspection"
        );
    }
    let payload = header
        .get("payload")
        .context("Codex session header has no payload")?;
    if payload.get("id").and_then(|value| value.as_str()) != Some(expected_session_id) {
        anyhow::bail!("Codex session header id does not match its rollout filename");
    }
    if let Some(version) = payload.get("cli_version") {
        let version = version
            .as_str()
            .context("Codex session header cli_version is not text")?;
        if version.is_empty() || version.len() > 128 || version.contains('\0') {
            anyhow::bail!("Codex session header has an invalid cli_version");
        }
    }
    let timestamp = payload
        .get("timestamp")
        .or_else(|| header.get("timestamp"))
        .map(|value| {
            let value = value
                .as_str()
                .context("Codex session_meta timestamp is not text")?;
            DateTime::parse_from_rfc3339(value)
                .map(|timestamp| timestamp.timestamp())
                .context("Codex session_meta timestamp is not RFC3339")
        })
        .transpose()?;
    Ok(timestamp)
}

#[cfg_attr(windows, allow(unused_variables))]
fn discover_codex(
    since: Option<i64>,
    deadline: Instant,
    rejected: &mut Vec<(String, DiscoveryRejection)>,
) -> Result<Vec<Candidate>> {
    ensure_before_deadline(deadline)?;
    let Some(root) = codex_sessions_root() else {
        return Ok(Vec::new());
    };
    let adapter = agent_for(AgentKind::Codex);
    let archive_root = root.with_file_name("archived_sessions");
    let directory = open_provider_directory_for_discovery(adapter, &root)?;
    let archive_directory = open_provider_directory_for_discovery(adapter, &archive_root)?;
    let mut seen = std::collections::HashSet::new();
    #[cfg(unix)]
    {
        let mut scanned = 0usize;
        let mut header_bytes = 0u64;
        let mut selection = CodexDiscoverySelection {
            rejected,
            seen: &mut seen,
        };
        let mut candidates = if let Some(directory) = directory.as_ref() {
            discover_codex_unix(
                &root,
                directory,
                since,
                deadline,
                &mut scanned,
                &mut header_bytes,
                &mut selection,
            )?
        } else {
            Vec::new()
        };
        if let Some(archive_directory) = archive_directory.as_ref() {
            // The flat archive has its own entry budget; active history must
            // not spend the allowance before archive traversal starts.
            scanned = 0;
            candidates.extend(discover_codex_archive_unix(
                &archive_root,
                archive_directory,
                since,
                deadline,
                &mut scanned,
                &mut header_bytes,
                &mut selection,
            )?);
        }
        order_codex_discovery_candidates(&mut candidates);
        Ok(candidates)
    }
    #[cfg(not(unix))]
    {
        // On Windows these pins use strict directory sharing. Discovery only
        // needs the existence proof here; release them before Codex can move
        // a completed rollout into archived_sessions.
        let active_exists = directory.is_some();
        let archive_exists = archive_directory.is_some();
        drop(directory);
        drop(archive_directory);
        let pinned_root = root.clone();
        let mut scanned = 0usize;
        let mut header_bytes = 0u64;
        let mut candidates = Vec::new();
        if active_exists {
            for year in read_codex_discovery_directory(&pinned_root, deadline, &mut scanned)? {
                let Some(year_name) = year.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if year_name.len() != 4 || !year_name.bytes().all(|byte| byte.is_ascii_digit()) {
                    continue;
                }
                let Some(year_number) = year_name.parse::<i32>().ok().filter(|year| *year > 0)
                else {
                    continue;
                };
                if !codex_discovery_real_directory(&year)? {
                    continue;
                }
                for month in read_codex_discovery_directory(&year.path(), deadline, &mut scanned)? {
                    let Some(month_name) = month.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    let Some(month_number) = month_name
                        .parse::<u32>()
                        .ok()
                        .filter(|month| month_name.len() == 2 && (1..=12).contains(month))
                    else {
                        continue;
                    };
                    if !codex_discovery_real_directory(&month)? {
                        continue;
                    }
                    for day in
                        read_codex_discovery_directory(&month.path(), deadline, &mut scanned)?
                    {
                        let Some(day_name) = day.file_name().to_str().map(str::to_owned) else {
                            continue;
                        };
                        let valid_day = day_name.parse::<u32>().ok().is_some_and(|day| {
                            day_name.len() == 2
                                && chrono::NaiveDate::from_ymd_opt(year_number, month_number, day)
                                    .is_some()
                        });
                        if !valid_day {
                            continue;
                        }
                        if !codex_discovery_real_directory(&day)? {
                            continue;
                        }
                        for file in
                            read_codex_discovery_directory(&day.path(), deadline, &mut scanned)?
                        {
                            ensure_before_deadline(deadline)?;
                            let file_type = match file.file_type() {
                                Ok(kind) => kind,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                    continue;
                                }
                                Err(error) => {
                                    return Err(error)
                                        .context("inspect Codex rollout discovery entry");
                                }
                            };
                            if file_type.is_symlink() {
                                anyhow::bail!(
                                    "Codex discovery encountered a symlinked rollout (fail-closed)"
                                );
                            }
                            if !file_type.is_file() {
                                continue;
                            }
                            let file_name = file.file_name();
                            let pinned_path = file.path();
                            if pinned_path.extension().and_then(|value| value.to_str())
                                != Some("jsonl")
                                || !file_name.to_string_lossy().starts_with("rollout-")
                            {
                                continue;
                            }
                            let logical_path = root
                                .join(&year_name)
                                .join(&month_name)
                                .join(&day_name)
                                .join(&file_name);
                            let Some(provider_session_id) =
                                codex_session_id_from_filename(&logical_path)
                            else {
                                continue;
                            };
                            if !seen.insert(provider_session_id.clone()) {
                                continue;
                            }
                            if since.is_some() {
                                let opened = match codex_discovery_open_file(&pinned_path) {
                                    Ok(opened) => opened,
                                    Err(error)
                                        if error.downcast_ref::<std::io::Error>().is_some_and(
                                            |cause| cause.kind() == std::io::ErrorKind::NotFound,
                                        ) =>
                                    {
                                        seen.remove(&provider_session_id);
                                        continue;
                                    }
                                    Err(error) => return Err(error),
                                };
                                if !codex_discovery_filter(
                                    opened,
                                    &provider_session_id,
                                    since,
                                    &mut header_bytes,
                                    rejected,
                                )? {
                                    continue;
                                }
                                ensure_before_deadline(deadline)?;
                            }
                            candidates.push(Candidate {
                                kind: AgentKind::Codex,
                                provider_session_id,
                                path: Some(logical_path),
                            });
                        }
                    }
                }
            }
        }
        if archive_exists {
            scanned = 0;
            for file in read_codex_discovery_directory(&archive_root, deadline, &mut scanned)? {
                ensure_before_deadline(deadline)?;
                let file_type = match file.file_type() {
                    Ok(kind) => kind,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error).context("inspect archived Codex rollout"),
                };
                if file_type.is_symlink() {
                    anyhow::bail!(
                        "Codex discovery encountered a symlinked archived rollout (fail-closed)"
                    );
                }
                if !file_type.is_file() {
                    continue;
                }
                let path = file.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl")
                    || !file.file_name().to_string_lossy().starts_with("rollout-")
                {
                    continue;
                }
                let Some(provider_session_id) = codex_session_id_from_filename(&path) else {
                    continue;
                };
                if !seen.insert(provider_session_id.clone()) {
                    continue;
                }
                if since.is_some() {
                    let opened = match codex_discovery_open_file(&path) {
                        Ok(opened) => opened,
                        Err(error)
                            if error.downcast_ref::<std::io::Error>().is_some_and(|cause| {
                                cause.kind() == std::io::ErrorKind::NotFound
                            }) =>
                        {
                            seen.remove(&provider_session_id);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    if !codex_discovery_filter(
                        opened,
                        &provider_session_id,
                        since,
                        &mut header_bytes,
                        rejected,
                    )? {
                        continue;
                    }
                    ensure_before_deadline(deadline)?;
                }
                candidates.push(Candidate {
                    kind: AgentKind::Codex,
                    provider_session_id,
                    path: Some(path),
                });
            }
        }
        order_codex_discovery_candidates(&mut candidates);
        Ok(candidates)
    }
}

fn order_codex_discovery_candidates(candidates: &mut Vec<Candidate>) {
    let archived = |candidate: &Candidate| {
        candidate
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "archived_sessions")
    };
    // An active copy is authoritative when Codex is between archive move
    // phases. Within either root, the rollout timestamp in the filename
    // provides a deterministic newest-first tie break for duplicate IDs.
    candidates.sort_by(|left, right| {
        left.provider_session_id
            .cmp(&right.provider_session_id)
            .then_with(|| archived(left).cmp(&archived(right)))
            .then_with(|| right.path.cmp(&left.path))
    });
    candidates.dedup_by(|left, right| left.provider_session_id == right.provider_session_id);
}

fn codex_discovery_time_matches(session_time: Option<i64>, since: Option<i64>) -> Result<bool> {
    match (session_time, since) {
        (Some(timestamp), Some(boundary)) => Ok(timestamp >= boundary),
        (None, Some(_)) => Err(CodexDiscoveryUnknownTime.into()),
        (_, None) => Ok(true),
    }
}

fn codex_discovery_filter(
    file: std::fs::File,
    session_id: &str,
    since: Option<i64>,
    header_bytes: &mut u64,
    rejected: &mut Vec<(String, DiscoveryRejection)>,
) -> Result<bool> {
    let Some(boundary) = since else {
        return Ok(true);
    };
    let outcome = codex_discovery_session_time(file, session_id, header_bytes)
        .and_then(|timestamp| codex_discovery_time_matches(timestamp, Some(boundary)));
    match outcome {
        Ok(include) => Ok(include),
        Err(error)
            if error
                .downcast_ref::<CodexDiscoveryBudgetExceeded>()
                .is_some() =>
        {
            Err(error)
        }
        Err(error) => {
            let fallback = if error.downcast_ref::<std::io::Error>().is_some() {
                DiscoveryRejection::CodexSourceUnreadable
            } else {
                DiscoveryRejection::CodexProviderRoot
            };
            record_codex_discovery_rejection(
                rejected,
                session_id,
                DiscoveryRejection::from_provider_error(&error, fallback),
            )?;
            Ok(false)
        }
    }
}

fn record_codex_discovery_rejection(
    rejected: &mut Vec<(String, DiscoveryRejection)>,
    session_id: &str,
    reason: DiscoveryRejection,
) -> Result<()> {
    if rejected.len() >= CODEX_DISCOVERY_REJECTION_CAP {
        return Err(CodexDiscoveryBudgetExceeded.into());
    }
    rejected.push((session_id.to_owned(), reason));
    Ok(())
}

#[cfg(windows)]
fn codex_discovery_open_file(path: &Path) -> Result<std::fs::File> {
    let parent = path
        .parent()
        .context("Codex rollout has no parent directory")?;
    let name = path.file_name().context("Codex rollout has no filename")?;
    let pinned = crate::utils::windows_fs::open_tree(parent)
        .context("pin Codex rollout parent without reparse points")?;
    crate::utils::windows_fs::open_live_source_at(&pinned, name)
        .context("open Codex rollout without following a reparse point")
}

#[cfg(not(any(unix, windows)))]
fn codex_discovery_open_file(path: &Path) -> Result<std::fs::File> {
    std::fs::File::open(path).context("open Codex rollout for bounded header validation")
}

#[cfg(unix)]
struct CodexDiscoverySelection<'a> {
    rejected: &'a mut Vec<(String, DiscoveryRejection)>,
    seen: &'a mut std::collections::HashSet<String>,
}

#[cfg(unix)]
fn discover_codex_unix(
    logical_root: &Path,
    directory: &std::fs::File,
    since: Option<i64>,
    deadline: Instant,
    scanned: &mut usize,
    header_bytes: &mut u64,
    selection: &mut CodexDiscoverySelection<'_>,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for year_name in read_discovery_names(directory, "Codex rollout", deadline, scanned)? {
        let Some(year_text) = year_name.to_str() else {
            continue;
        };
        if year_text.len() != 4 || !year_text.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Some(year_number) = year_text.parse::<i32>().ok().filter(|year| *year > 0) else {
            continue;
        };
        let Some(year) =
            open_discovery_entry_at(directory, &year_name, DiscoveryEntryKind::Directory)?
        else {
            continue;
        };
        for month_name in read_discovery_names(&year, "Codex rollout", deadline, scanned)? {
            let Some(month_text) = month_name.to_str() else {
                continue;
            };
            let Some(month_number) = month_text
                .parse::<u32>()
                .ok()
                .filter(|month| month_text.len() == 2 && (1..=12).contains(month))
            else {
                continue;
            };
            let Some(month) =
                open_discovery_entry_at(&year, &month_name, DiscoveryEntryKind::Directory)?
            else {
                continue;
            };
            for day_name in read_discovery_names(&month, "Codex rollout", deadline, scanned)? {
                let Some(day_text) = day_name.to_str() else {
                    continue;
                };
                let valid_day = day_text.parse::<u32>().ok().is_some_and(|day| {
                    day_text.len() == 2
                        && chrono::NaiveDate::from_ymd_opt(year_number, month_number, day).is_some()
                });
                if !valid_day {
                    continue;
                }
                let Some(day) =
                    open_discovery_entry_at(&month, &day_name, DiscoveryEntryKind::Directory)?
                else {
                    continue;
                };
                for file_name in read_discovery_names(&day, "Codex rollout", deadline, scanned)? {
                    ensure_before_deadline(deadline)?;
                    let logical_path = logical_root
                        .join(&year_name)
                        .join(&month_name)
                        .join(&day_name)
                        .join(&file_name);
                    if logical_path.extension().and_then(|value| value.to_str()) != Some("jsonl")
                        || !file_name.to_string_lossy().starts_with("rollout-")
                    {
                        continue;
                    }
                    let Some(provider_session_id) = codex_session_id_from_filename(&logical_path)
                    else {
                        continue;
                    };
                    if !selection.seen.insert(provider_session_id.clone()) {
                        continue;
                    }
                    let Some(file) =
                        open_discovery_entry_at(&day, &file_name, DiscoveryEntryKind::File)?
                    else {
                        selection.seen.remove(&provider_session_id);
                        continue;
                    };
                    if !codex_discovery_filter(
                        file,
                        &provider_session_id,
                        since,
                        header_bytes,
                        selection.rejected,
                    )? {
                        continue;
                    }
                    ensure_before_deadline(deadline)?;
                    candidates.push(Candidate {
                        kind: AgentKind::Codex,
                        provider_session_id,
                        path: Some(logical_path),
                    });
                }
            }
        }
    }
    Ok(candidates)
}

#[cfg(unix)]
fn discover_codex_archive_unix(
    logical_root: &Path,
    directory: &std::fs::File,
    since: Option<i64>,
    deadline: Instant,
    scanned: &mut usize,
    header_bytes: &mut u64,
    selection: &mut CodexDiscoverySelection<'_>,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for name in read_discovery_names(directory, "Codex archive", deadline, scanned)? {
        ensure_before_deadline(deadline)?;
        let logical_path = logical_root.join(&name);
        if logical_path.extension().and_then(|value| value.to_str()) != Some("jsonl")
            || !name.to_string_lossy().starts_with("rollout-")
        {
            continue;
        }
        let Some(provider_session_id) = codex_session_id_from_filename(&logical_path) else {
            continue;
        };
        if !selection.seen.insert(provider_session_id.clone()) {
            continue;
        }
        let Some(file) = open_discovery_entry_at(directory, &name, DiscoveryEntryKind::File)?
        else {
            selection.seen.remove(&provider_session_id);
            continue;
        };
        let include = codex_discovery_filter(
            file,
            &provider_session_id,
            since,
            header_bytes,
            selection.rejected,
        )?;
        ensure_before_deadline(deadline)?;
        if !include {
            continue;
        }
        candidates.push(Candidate {
            kind: AgentKind::Codex,
            provider_session_id,
            path: Some(logical_path),
        });
    }
    Ok(candidates)
}

#[cfg(not(unix))]
fn read_codex_discovery_directory(
    directory: &Path,
    deadline: Instant,
    scanned: &mut usize,
) -> Result<Vec<std::fs::DirEntry>> {
    ensure_before_deadline(deadline)?;
    let read = std::fs::read_dir(directory)
        .with_context(|| format!("read Codex rollout directory {}", directory.display()))?;
    ensure_before_deadline(deadline)?;
    let mut entries = Vec::new();
    for entry in read {
        ensure_before_deadline(deadline)?;
        *scanned = scanned
            .checked_add(1)
            .context("Codex discovery entry counter overflow")?;
        if *scanned > 20_000 {
            return Err(CodexDiscoveryEntryBudgetExceeded.into());
        }
        entries.push(
            entry
                .with_context(|| format!("read Codex rollout entry in {}", directory.display()))?,
        );
        ensure_before_deadline(deadline)?;
    }
    entries.sort_unstable_by(|left, right| right.file_name().cmp(&left.file_name()));
    Ok(entries)
}

#[cfg(not(unix))]
fn codex_discovery_real_directory(entry: &std::fs::DirEntry) -> Result<bool> {
    let file_type = entry
        .file_type()
        .context("inspect Codex rollout directory entry")?;
    if file_type.is_symlink() {
        anyhow::bail!("Codex discovery encountered a symlinked directory (fail-closed)");
    }
    Ok(file_type.is_dir())
}

fn parse_since(value: Option<&str>) -> CliResult<Option<i64>> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|time| time.timestamp())
                .map_err(|_| {
                    CliError::command_usage("--since must be a valid RFC3339 timestamp")
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                })
        })
        .transpose()
}

/// Argument-only selector, validated before any provider filesystem access.
#[derive(Debug)]
enum DiscoverySelector {
    Path(Candidate),
    Session {
        session_id: String,
        filter: Option<AgentKind>,
    },
    Batch {
        filter: Option<AgentKind>,
        since: Option<i64>,
    },
}

/// Validate argv in the documented order. The parent runs this before it
/// spawns the discovery helper so each argv error keeps its fixed actionable
/// message, and the helper re-runs the same function so they cannot drift.
fn validate_discovery_selector(args: &ImportArgs) -> CliResult<DiscoverySelector> {
    if args.limit == 0 || args.limit > MAX_IMPORT_LIMIT {
        return Err(CliError::command_usage("--limit must be between 1 and 100")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let filter = args.agent.as_deref().map(importable_kind).transpose()?;
    if let Some(path) = args.path.clone() {
        let kind = filter.ok_or_else(|| {
            CliError::command_usage("--path requires --agent")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        return Ok(DiscoverySelector::Path(path_candidate(path, kind)?));
    }
    if filter == Some(AgentKind::Hermes) && args.path.is_none() {
        return Err(CliError::command_usage(
            "Hermes import requires --path to one <session-id>.jsonl export and does not scan a Hermes home",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if filter == Some(AgentKind::Pi) && args.path.is_none() {
        return Err(CliError::command_usage(
            "Pi import requires --path to one <timestamp>_<session-id>.jsonl session file and does not scan Pi's session directory",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if let Some(session_id) = args.session.as_deref() {
        if let Some(kind) = filter {
            validate_session_id_for_kind(session_id, kind)?;
        } else if !session_id_is_valid_for_kind(session_id, AgentKind::ClaudeCode)
            && !session_id_is_valid_for_kind(session_id, AgentKind::Codex)
        {
            return Err(CliError::command_usage(
                "invalid provider session id (expected a safe Claude or Codex session identifier)",
            )
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        return Ok(DiscoverySelector::Session {
            session_id: session_id.to_string(),
            filter,
        });
    }
    if filter == Some(AgentKind::OpenCode) {
        return Err(CliError::command_usage(
            "OpenCode batch discovery is unavailable; select a session explicitly with --session",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let since = parse_since(args.since.as_deref())?;
    Ok(DiscoverySelector::Batch { filter, since })
}

fn ensure_discovery_before_deadline(deadline: Instant) -> Result<(), DiscoveryRejection> {
    ensure_before_deadline(deadline).map_err(|_| DiscoveryRejection::DeadlineExceeded)
}

fn discover(
    args: &ImportArgs,
    repo_root: &Path,
    deadline: Instant,
) -> Result<DiscoveryBatch, DiscoveryRejection> {
    ensure_discovery_before_deadline(deadline)?;
    let (filter, since) = match validate_discovery_selector(args)
        .map_err(|_| DiscoveryRejection::SelectorRejected)?
    {
        DiscoverySelector::Path(candidate) => return Ok((vec![candidate], None, Vec::new())),
        DiscoverySelector::Session { session_id, filter } => {
            return discover_session(&session_id, filter, repo_root, deadline)
                .map(|candidates| (candidates, None, Vec::new()));
        }
        DiscoverySelector::Batch { filter, since } => (filter, since),
    };
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    if filter.is_none() || filter == Some(AgentKind::ClaudeCode) {
        candidates.extend(
            discover_claude(repo_root, since, deadline).map_err(|error| {
                DiscoveryRejection::from_provider_error(
                    &error,
                    DiscoveryRejection::ClaudeProviderRoot,
                )
            })?,
        );
    }
    if filter.is_none() || filter == Some(AgentKind::Codex) {
        candidates.extend(
            discover_codex(since, deadline, &mut rejected).map_err(|error| {
                DiscoveryRejection::from_provider_error(
                    &error,
                    DiscoveryRejection::CodexProviderRoot,
                )
            })?,
        );
    }
    ensure_discovery_before_deadline(deadline)?;
    candidates.sort_by(|left, right| {
        (left.kind.as_db_str(), left.provider_session_id.as_str())
            .cmp(&(right.kind.as_db_str(), right.provider_session_id.as_str()))
    });
    candidates.dedup_by(|left, right| {
        left.kind == right.kind && left.provider_session_id == right.provider_session_id
    });
    ensure_discovery_before_deadline(deadline)?;
    let offset = args.cursor.unwrap_or(0);
    if offset > candidates.len() {
        return Err(DiscoveryRejection::CursorOutOfRange);
    }
    let end = offset.saturating_add(args.limit).min(candidates.len());
    let next_cursor = (end < candidates.len()).then_some(end);
    // Keep failures visible on every page: a caller that resumes with a
    // cursor must not silently treat an unfilterable session as excluded.
    Ok((candidates[offset..end].to_vec(), next_cursor, rejected))
}

fn discover_session(
    session_id: &str,
    filter: Option<AgentKind>,
    repo_root: &Path,
    deadline: Instant,
) -> Result<Vec<Candidate>, DiscoveryRejection> {
    if filter == Some(AgentKind::OpenCode) {
        return Ok(vec![Candidate {
            kind: AgentKind::OpenCode,
            provider_session_id: session_id.to_string(),
            path: None,
        }]);
    }
    let kinds = filter
        .map(|kind| vec![kind])
        .unwrap_or_else(|| vec![AgentKind::ClaudeCode, AgentKind::Codex]);
    let mut candidates = Vec::new();
    for kind in kinds {
        if !session_id_is_valid_for_kind(session_id, kind) {
            continue;
        }
        let path = match kind {
            AgentKind::ClaudeCode => {
                let found = resolve_session_file(repo_root, session_id).map_err(|error| {
                    DiscoveryRejection::from_provider_error(
                        &error,
                        DiscoveryRejection::ClaudeProviderRoot,
                    )
                })?;
                ensure_discovery_before_deadline(deadline)?;
                found
            }
            AgentKind::Codex => {
                let found = find_codex_rollout_for_import(session_id).map_err(|error| {
                    DiscoveryRejection::from_provider_error(
                        &error,
                        DiscoveryRejection::CodexProviderRoot,
                    )
                })?;
                ensure_discovery_before_deadline(deadline)?;
                found
            }
            _ => None,
        };
        if let Some(path) = path {
            candidates.push(Candidate {
                kind,
                provider_session_id: session_id.to_string(),
                path: Some(path),
            });
        }
    }
    if candidates.len() > 1 {
        return Err(DiscoveryRejection::AmbiguousSession);
    }
    if candidates.is_empty() {
        return Err(DiscoveryRejection::SessionNotFound);
    }
    Ok(candidates)
}

/// Private subprocess entry used to make provider discovery killable at the
/// import command's absolute deadline. The frame is JSON only between two
/// copies of the same Libra binary and is never a public machine schema.
#[doc(hidden)]
pub fn run_import_discovery_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: DiscoveryHelperRequest =
        serde_json::from_slice(input).context("decode internal import discovery request")?;
    let deadline = Instant::now()
        + Duration::from_millis(
            request
                .remaining_ms
                .min(IMPORT_TOTAL_DEADLINE.as_millis() as u64),
        );
    let args = ImportArgs {
        session: request.session,
        path: request.path.map(WirePath::into_path_buf),
        since: request.since,
        all: request.all,
        agent: request.agent,
        limit: request.limit,
        cursor: request.cursor,
        yes: true,
        restore_erased: false,
        codex_index_backup: None,
    };
    let repo_root = request.repo_root.into_path_buf();
    let response = match discover(&args, &repo_root, deadline) {
        Ok((candidates, next_cursor, rejected)) => DiscoveryHelperResponse::Ok {
            candidates: candidates
                .into_iter()
                .map(|candidate| DiscoveryCandidateWire {
                    kind: candidate.kind.as_cli_slug().to_string(),
                    provider_session_id: candidate.provider_session_id,
                    path: candidate.path.as_deref().map(WirePath::from_path),
                })
                .collect(),
            next_cursor,
            rejected,
        },
        // The helper crosses a process boundary specifically to contain
        // provider filesystem work. Only the closed reason crosses it; never
        // an error chain from that process.
        Err(reason) => DiscoveryHelperResponse::Error { reason },
    };
    serde_json::to_vec(&response).context("encode internal import discovery response")
}

async fn discover_bounded(
    args: &ImportArgs,
    repo_root: &Path,
    deadline: Instant,
) -> CliResult<DiscoveryBatch> {
    ensure_discovery_before_deadline(deadline).map_err(DiscoveryRejection::into_cli_error)?;
    // Reject argv-only selector errors here, with their actionable messages,
    // before a helper is spawned; the helper re-validates the same argv.
    validate_discovery_selector(args)?;
    let remaining_ms = u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let request = DiscoveryHelperRequest {
        repo_root: WirePath::from_path(repo_root),
        session: args.session.clone(),
        path: args.path.as_deref().map(WirePath::from_path),
        since: args.since.clone(),
        all: args.all,
        agent: args.agent.clone(),
        limit: args.limit,
        cursor: args.cursor,
        remaining_ms,
    };
    let request = serde_json::to_vec(&request).map_err(|error| {
        CliError::fatal(format!(
            "failed to encode bounded import discovery: {error}"
        ))
    })?;
    if request.len() as u64 > IMPORT_DISCOVERY_HELPER_FRAME_CAP {
        return Err(CliError::fatal(
            "bounded import discovery request exceeded its internal frame limit",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let command = require_registered_import_helper_command(registered_helper_command(
        IMPORT_DISCOVERY_HELPER_ARG,
    ))
    .map_err(|_| {
        CliError::fatal("agent import discovery helper is unavailable in this host")
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing)
    })?;
    let output = match run_registered_bounded_helper_until(
        command,
        &[&request],
        IMPORT_DISCOVERY_HELPER_FRAME_CAP,
        deadline,
    )
    .await
    {
        RegisteredHelperOutput::Output(output) => output,
        RegisteredHelperOutput::DeadlineExceeded => {
            return Err(CliError::fatal(
                "agent import discovery exceeded its total execution deadline",
            )
            .with_stable_code(StableErrorCode::AgentImportPartialBatch));
        }
        RegisteredHelperOutput::Failed => {
            return Err(CliError::fatal(
                "bounded import discovery helper did not return a usable response",
            )
            .with_stable_code(StableErrorCode::AgentImportPartialBatch));
        }
    };
    let invalid_response = || {
        CliError::fatal("bounded import discovery helper returned an invalid response")
            .with_stable_code(StableErrorCode::AgentImportPartialBatch)
    };
    let response: DiscoveryHelperResponse =
        serde_json::from_slice(&output).map_err(|_| invalid_response())?;
    match response {
        DiscoveryHelperResponse::Ok {
            candidates,
            next_cursor,
            rejected,
        } => {
            let candidates = candidates
                .into_iter()
                .map(|candidate| {
                    let kind =
                        importable_kind_from_slug(&candidate.kind).ok_or_else(invalid_response)?;
                    Ok(Candidate {
                        kind,
                        provider_session_id: candidate.provider_session_id,
                        path: candidate.path.map(WirePath::into_path_buf),
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;
            Ok((candidates, next_cursor, rejected))
        }
        DiscoveryHelperResponse::Error { reason } => Err(reason.into_cli_error()),
    }
}

#[cfg(unix)]
fn wait_for_consent_fd(fd: std::os::fd::RawFd, deadline: Instant) -> std::io::Result<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the supplied
        // live fd and remains valid for the duration of poll.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready > 0 {
            return Ok(true);
        }
        if ready == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg_attr(windows, allow(unused_variables, unreachable_code))]
fn require_consent(
    args: &ImportArgs,
    output: &OutputConfig,
    candidate_count: Option<usize>,
    deadline: Instant,
) -> CliResult<()> {
    if !output.quiet {
        eprintln!("{}", super::CIPHERTEXT_ARCHIVE_DISCLOSURE);
    }
    if args.yes {
        return Ok(());
    }
    if output.is_json() || !io::stdin().is_terminal() {
        return Err(CliError::command_usage(
            "agent import reads private provider session data; rerun with --yes after reviewing the selected scope",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    eprint!(
        "Import scope: agent={}, current repository only, {} candidate(s) (limit {}). \
         Libra will read private provider sessions, redact typed fields, and write projections \
         to refs/libra/traces; a later `libra agent push` may upload those redacted traces. \
         Continue? [y/N] ",
        args.agent
            .as_deref()
            .unwrap_or(if args.codex_index_backup.is_some() {
                "codex"
            } else {
                "claude-code/codex"
            }),
        match candidate_count {
            Some(count) if args.codex_index_backup.is_some() => format!("up to {count}"),
            Some(count) => count.to_string(),
            None => format!("up to {}", args.limit),
        },
        args.limit,
    );
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let ready = wait_for_consent_fd(io::stdin().as_raw_fd(), deadline).map_err(|error| {
            CliError::fatal(format!("failed to wait for import confirmation: {error}"))
        })?;
        if !ready {
            return Err(CliError::fatal(
                "agent import confirmation exceeded its total execution deadline",
            )
            .with_stable_code(StableErrorCode::AgentImportPartialBatch));
        }
    }
    #[cfg(not(unix))]
    {
        return Err(CliError::command_usage(
            "bounded interactive import confirmation is unavailable on this platform; rerun with --yes",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliError::fatal(format!("failed to read import confirmation: {error}")))?;
    if Instant::now() >= deadline {
        return Err(CliError::fatal(
            "agent import confirmation exceeded its total execution deadline",
        )
        .with_stable_code(StableErrorCode::AgentImportPartialBatch));
    }
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(
            CliError::command_usage("agent import cancelled before reading session content")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        )
    }
}

/// A Pi session file the CLI opened for the preparation helper. It has no
/// `Read` or `Seek` implementation and no accessor, so the CLI process cannot
/// read its bytes: the one thing it can do is become the standard input of
/// the killable helper (`into_helper_stdin`, called once, in the helper
/// spawn itself), which reads it under the import deadline. The CLI never
/// holds the file as a `File`.
mod pi_held_source {
    pub(super) struct PiHeldSource(std::fs::File);

    impl PiHeldSource {
        /// Open `name` under `parent`: the directory and the file without
        /// following a symlink, and only a regular file. No byte is taken.
        pub(super) fn open(parent: &std::path::Path, name: &std::ffi::OsStr) -> Option<Self> {
            let directory = crate::utils::object::open_directory_tree_no_follow(parent).ok()?;
            let file =
                crate::utils::object::open_regular_file_at_no_follow(&directory, name).ok()?;
            file.metadata().ok()?.is_file().then_some(Self(file))
        }

        /// Make the descriptor the preparation helper's standard input.
        pub(super) fn into_helper_stdin(self) -> std::process::Stdio {
            std::process::Stdio::from(self.0)
        }
    }
}

use pi_held_source::PiHeldSource;

/// Open one explicitly named Pi session file, after consent: a regular file
/// opened under its parent directory without following a symlink. Nothing
/// is read here. The descriptor goes to the killable preparation helper,
/// which reads it strictly bounded under the import deadline, as it reads a
/// held Claude or Codex transcript; the snapshot, the frame, and the header
/// checks (the session id, a `cwd` inside the repository) run there. The
/// source identity is the file itself: another Pi profile's file with the
/// same custom id is a different source, so the shared V2 ownership check
/// refuses to join it to this session. Only the repository-keyed HMAC of the
/// path is ever stored. Every failure is content-free.
#[cfg_attr(windows, allow(dead_code))]
fn open_pi_session_file(candidate: &Candidate) -> Result<(PiHeldSource, String)> {
    let path = candidate
        .path
        .as_ref()
        .context("Pi import requires one explicit session file path")?;
    let source_id = path
        .canonicalize()
        .map_err(|_| ImportError::SourceAuthorization)?
        .to_str()
        .ok_or(ImportError::SourceAuthorization)?
        .to_string();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(ImportError::SourceAuthorization)?;
    let name = path.file_name().ok_or(ImportError::SourceAuthorization)?;
    #[cfg(test)]
    let _ = PI_SOURCE_READS.try_with(|count| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    // The file is opened inside `pi_held_source`, so no raw handle to it
    // ever exists in this process outside that module.
    let held = PiHeldSource::open(parent, name).ok_or(ImportError::SourceAuthorization)?;
    Ok((held, source_id))
}

#[cfg_attr(windows, allow(dead_code))]
async fn resolve_candidate_source(
    candidate: &Candidate,
    repo_root: &Path,
    deadline: Instant,
) -> Result<(TranscriptSource, String, String)> {
    #[cfg(all(test, feature = "test-live-agent"))]
    let _ = INDEX_SOURCE_READS.try_with(|count| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    if candidate.kind == AgentKind::Hermes {
        let path = candidate
            .path
            .as_ref()
            .context("Hermes import requires one explicit JSONL path")?;
        let scope = crate::internal::ai::observed_agents::builtin::hermes::HermesScope {
            session_id: candidate.provider_session_id.clone(),
            profile: "default".to_string(),
            repo_root: repo_root.to_path_buf(),
        };
        let bytes =
            crate::internal::ai::observed_agents::builtin::hermes::authorized_hermes_jsonl_bytes(
                path,
                &scope,
                &crate::internal::ai::observed_agents::HermesReadLimits::default(),
            )
            .ok_or(ImportError::SourceAuthorization)?;
        let session_id = crate::internal::ai::hooks::runtime::build_ai_session_id(
            "hermes",
            &candidate.provider_session_id,
        );
        let auth = ExportAuthorized::issue("hermes", &session_id, &bytes);
        return Ok((
            TranscriptSource::Bytes { bytes, auth },
            "file".to_string(),
            candidate.provider_session_id.clone(),
        ));
    }
    if candidate.kind == AgentKind::OpenCode {
        let session_id = crate::internal::ai::hooks::runtime::build_ai_session_id(
            "opencode",
            &candidate.provider_session_id,
        );
        let source = authorized_trusted_sandboxed_export_until(
            &candidate.provider_session_id,
            &session_id,
            repo_root,
            ExportLimits::default(),
            deadline,
        )
        .await?;
        return Ok((
            source,
            "export".to_string(),
            candidate.provider_session_id.clone(),
        ));
    }
    let path = candidate
        .path
        .as_ref()
        .context("file-backed import candidate has no path")?;
    let adapter = agent_for(candidate.kind);
    let ctx = AgentSessionCtx {
        session_id: crate::internal::ai::hooks::runtime::build_ai_session_id(
            provider_name(candidate.kind),
            &candidate.provider_session_id,
        ),
        provider_session_id: candidate.provider_session_id.clone(),
        working_dir: repo_root.to_path_buf(),
        transcript_path: Some(path.clone()),
    };
    let source = resolve_import_transcript_source_until(adapter, &ctx, deadline)
        .map_err(|_| ImportError::SourceAuthorization)?
        .ok_or(ImportError::SourceAuthorization)?;
    ensure_before_deadline(deadline)?;
    let source_id = match &source {
        TranscriptSource::File { source_id, .. } => source_id.clone(),
        TranscriptSource::Bytes { .. } => candidate.provider_session_id.clone(),
    };
    Ok((source, "file".to_string(), source_id))
}

fn preparation_error_kind(error: &anyhow::Error) -> Option<PreparationImportErrorKind> {
    match error.downcast_ref::<ImportError>()? {
        ImportError::WorkingDirMissingOrAmbiguous => {
            Some(PreparationImportErrorKind::WorkingDirMissingOrAmbiguous)
        }
        ImportError::RepositoryConflict => Some(PreparationImportErrorKind::RepositoryConflict),
        ImportError::SessionIdentityConflict => {
            Some(PreparationImportErrorKind::SessionIdentityConflict)
        }
        ImportError::Erased => Some(PreparationImportErrorKind::Erased),
        ImportError::LeaseBusy => Some(PreparationImportErrorKind::LeaseBusy),
        ImportError::SourceAuthorization => Some(PreparationImportErrorKind::SourceAuthorization),
        ImportError::AuthorizedReaderUnavailable | ImportError::AuthorizedReaderFailed => {
            Some(PreparationImportErrorKind::SourceAuthorization)
        }
        ImportError::NoImportableTurns => Some(PreparationImportErrorKind::NoImportableTurns),
        ImportError::BatchInputLimit => Some(PreparationImportErrorKind::BatchInputLimit),
        ImportError::DeadlineExceeded => Some(PreparationImportErrorKind::DeadlineExceeded),
        ImportError::FutureTimestamp => Some(PreparationImportErrorKind::FutureTimestamp),
    }
}

fn preparation_error(kind: PreparationImportErrorKind) -> ImportError {
    match kind {
        PreparationImportErrorKind::WorkingDirMissingOrAmbiguous => {
            ImportError::WorkingDirMissingOrAmbiguous
        }
        PreparationImportErrorKind::RepositoryConflict => ImportError::RepositoryConflict,
        PreparationImportErrorKind::SessionIdentityConflict => ImportError::SessionIdentityConflict,
        PreparationImportErrorKind::Erased => ImportError::Erased,
        PreparationImportErrorKind::LeaseBusy => ImportError::LeaseBusy,
        PreparationImportErrorKind::SourceAuthorization => ImportError::SourceAuthorization,
        PreparationImportErrorKind::NoImportableTurns => ImportError::NoImportableTurns,
        PreparationImportErrorKind::BatchInputLimit => ImportError::BatchInputLimit,
        PreparationImportErrorKind::DeadlineExceeded => ImportError::DeadlineExceeded,
        PreparationImportErrorKind::FutureTimestamp => ImportError::FutureTimestamp,
    }
}

fn descriptor_preparation_control_from_environment() -> Result<PreparationDescriptorControl> {
    let encoded = std::env::var(IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV)
        .context("read internal descriptor preparation control")?;
    if encoded.len() > 4 * 1024 {
        anyhow::bail!("internal descriptor preparation control exceeds its frame limit");
    }
    let control: PreparationDescriptorControl =
        serde_json::from_str(&encoded).context("decode internal descriptor preparation control")?;
    if control.read_cap > TRANSCRIPT_READ_HARD_CAP_BYTES
        || control.remaining_ms == 0
        || control
            .transport
            .as_ref()
            .is_some_and(|transport| transport.version != 1)
        || !matches!(control.source_kind.as_str(), "file" | "export")
        || importable_kind_from_slug(&control.agent_kind).is_none()
    {
        anyhow::bail!("invalid internal descriptor preparation control");
    }
    Ok(control)
}

/// Private descriptor helper entrypoint. `stdin` is a held source capability,
/// not a request frame. The only request-like material lives in the strictly
/// safe environment control parsed above.
#[doc(hidden)]
pub fn run_import_preparation_descriptor_helper_from_stdin() -> Result<Vec<u8>> {
    let control = descriptor_preparation_control_from_environment()?;
    let kind = AgentKind::from_cli_slug(&control.agent_kind)
        .context("invalid internal descriptor preparation agent kind")?;
    let mut stdin = std::io::stdin().lock();
    let (bytes, raw_bytes) = match read_strictly_bounded(&mut stdin, control.read_cap) {
        StrictBoundedRead::Complete(bytes) => {
            let raw_bytes =
                u64::try_from(bytes.len()).context("descriptor read length exceeds u64")?;
            (bytes, raw_bytes)
        }
        StrictBoundedRead::Oversize { observed_bytes } => (Vec::new(), observed_bytes),
        StrictBoundedRead::Failed { error, .. } => {
            return Err(error).context("read held descriptor for internal import preparation");
        }
    };
    let deadline = Instant::now()
        + Duration::from_millis(
            control
                .remaining_ms
                .min(IMPORT_TOTAL_DEADLINE.as_millis() as u64),
        );
    let outcome = if raw_bytes > control.read_cap {
        Err(ImportError::BatchInputLimit.into())
    } else {
        prepare_import_projection_with_reasoning(kind, control.provider_commitment, bytes, deadline)
    };
    let response = match outcome {
        Ok((projection, native)) => {
            let transport_required = native.as_ref().map(|_| true);
            let transport = if let Some(native) = native {
                let binding = PreparationImportTransportBinding::from_control(&control)?
                    .ok_or(ImportError::AuthorizedReaderFailed)?;
                let context =
                    binding.context(kind, native.transport_source_session(), &projection)?;
                let key = control
                    .transport
                    .ok_or(ImportError::AuthorizedReaderFailed)?
                    .key
                    .into_key()
                    .map_err(|_| ImportError::AuthorizedReaderFailed)?;
                Some(
                    seal_import_transport(key, &context, native)
                        .map_err(|_| ImportError::AuthorizedReaderFailed)?,
                )
            } else {
                None
            };
            PreparationDescriptorHelperResponse::Ok {
                projection: Box::new(projection),
                raw_bytes,
                transport_required,
                transport,
            }
        }
        Err(error) => PreparationDescriptorHelperResponse::Error {
            error_kind: preparation_error_kind(&error),
            raw_bytes,
        },
    };
    let encoded = encode_bounded_json(
        &response,
        usize::try_from(IMPORT_PREPARATION_HELPER_OUTPUT_CAP)?,
    )
    .map_err(|_| ImportError::AuthorizedReaderFailed)?;
    if encoded.len() as u64 > IMPORT_PREPARATION_HELPER_OUTPUT_CAP {
        return Err(ImportError::AuthorizedReaderFailed.into());
    }
    Ok(encoded)
}

/// Private subprocess entry for a durable import replay's foreground
/// object-index repair. The parent owns the kill deadline; the helper keeps
/// object-store reads and SQLite repair outside the long-lived CLI process.
#[doc(hidden)]
pub fn run_import_index_repair_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: IndexRepairHelperRequest =
        serde_json::from_slice(input).context("decode internal import index repair request")?;
    let storage_root = request.storage_root.into_path_buf();
    if request.session_id.is_empty()
        || request.session_id.len() > 256
        || !storage_root.join(util::DATABASE).is_file()
        || !storage_root.join("objects").is_dir()
    {
        anyhow::bail!("invalid internal import index repair target");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start internal import index repair runtime")?;
    let response = runtime.block_on(async {
        let conn = db::get_db_conn_instance_for_path(&storage_root.join(util::DATABASE))
            .await
            .context("open repository database for import index repair")?;
        Ok::<_, anyhow::Error>(
            match super::doctor::repair_session_object_index(
                &conn,
                &storage_root,
                super::doctor::SessionObjectIndexRepairRequest {
                    session_id: &request.session_id,
                    marker_owner: &request.marker_owner,
                    marker_generation: &request.marker_generation,
                    agent_kind: &request.agent_kind,
                    provider_session_id: &request.provider_session_id,
                    capture_scope: &request.capture_scope,
                },
            )
            .await
            {
                Ok(repaired_rows) => IndexRepairHelperResponse::Ok { repaired_rows },
                Err(_) => IndexRepairHelperResponse::Error {},
            },
        )
    })?;
    serde_json::to_vec(&response).context("encode internal import index repair response")
}

async fn invoke_import_index_repair_helper(
    storage_root: &Path,
    barrier: &ImportIndexBarrier,
    deadline: Instant,
) -> Result<usize> {
    ensure_before_deadline(deadline)?;
    let frame = serde_json::to_vec(&IndexRepairHelperRequest {
        storage_root: WirePath::from_path(storage_root),
        session_id: barrier.session_id.clone(),
        marker_owner: barrier.marker.owner.clone(),
        marker_generation: barrier.marker.generation.clone(),
        agent_kind: barrier.marker.agent_kind.clone(),
        provider_session_id: barrier.marker.provider_session_id.clone(),
        capture_scope: barrier.capture_scope.clone(),
    })
    .context("encode bounded import index repair request")?;
    if frame.len() as u64 > IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP {
        anyhow::bail!("bounded import index repair request exceeds its frame limit");
    }
    let command = require_registered_import_helper_command(registered_helper_command(
        IMPORT_INDEX_REPAIR_HELPER_ARG,
    ))?;
    let output = match run_registered_bounded_helper_until(
        command,
        &[&frame],
        IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP,
        deadline,
    )
    .await
    {
        RegisteredHelperOutput::Output(output) => output,
        RegisteredHelperOutput::DeadlineExceeded => {
            return Err(ImportError::DeadlineExceeded.into());
        }
        RegisteredHelperOutput::Failed => return Err(ImportError::AuthorizedReaderFailed.into()),
    };
    let response = match serde_json::from_slice(&output) {
        Ok(response) => response,
        Err(_) => anyhow::bail!("bounded import index repair helper returned an invalid response"),
    };
    match response {
        IndexRepairHelperResponse::Ok { repaired_rows } => Ok(repaired_rows),
        // Helper errors intentionally carry no provider/database error chain.
        IndexRepairHelperResponse::Error {} => {
            anyhow::bail!(
                "import object-index repair did not complete; run `libra agent doctor --repair`"
            )
        }
    }
}

fn parse_import_index_barrier_marker(value: &str) -> Result<ImportIndexRepairMarker> {
    let marker: ImportIndexRepairMarker =
        serde_json::from_str(value).context("decode durable import object-index barrier marker")?;
    validate_import_index_repair_marker(&marker)?;
    Ok(marker)
}

async fn lock_import_index_barrier_row<C: ConnectionTrait>(db: &C, session_id: &str) -> Result<()> {
    // This no-op UPDATE is deliberately the first mutation after the caller
    // has checked its workspace fence. On SQLite it obtains the writer slot
    // before we inspect the marker, preventing a read/overwrite race with
    // another process.
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE metadata_kv SET updated_at = updated_at
         WHERE scope = ? AND target = ? AND key = ?",
        [
            MetadataScope::AgentImportIndexRepair.as_str().into(),
            session_id.into(),
            IMPORT_INDEX_REPAIR_MARKER_KEY.into(),
        ],
    ))
    .await
    .context("lock import object-index barrier marker")?;
    Ok(())
}

fn marker_is_owned_by(marker: &ImportIndexRepairMarker, barrier: &ImportIndexBarrier) -> bool {
    marker.owner == barrier.marker.owner
        && marker.generation == barrier.marker.generation
        && marker.identity_id == barrier.marker.identity_id
        && marker.capture_scope.as_ref() == Some(&barrier.capture_scope)
}

async fn read_owned_import_index_barrier<C: ConnectionTrait>(
    db: &C,
    barrier: &ImportIndexBarrier,
) -> Result<ImportIndexRepairMarker> {
    let entry = MetadataKv::get_with_conn(
        db,
        MetadataScope::AgentImportIndexRepair,
        &barrier.session_id,
        IMPORT_INDEX_REPAIR_MARKER_KEY,
    )
    .await
    .context("read owned import object-index barrier marker")?
    .context("import object-index barrier ownership disappeared")?;
    let marker = parse_import_index_barrier_marker(&entry.value)?;
    if !marker_is_owned_by(&marker, barrier) {
        return Err(ImportError::LeaseBusy.into());
    }
    Ok(marker)
}

async fn persist_import_index_barrier<C: ConnectionTrait>(
    db: &C,
    session_id: &str,
    marker: &ImportIndexRepairMarker,
) -> Result<()> {
    let value = serde_json::to_string(marker).context("encode import object-index barrier")?;
    MetadataKv::set_with_conn(
        db,
        MetadataScope::AgentImportIndexRepair,
        session_id,
        IMPORT_INDEX_REPAIR_MARKER_KEY,
        &value,
        MetadataValueType::Text,
    )
    .await
    .context("persist import object-index barrier marker")?;
    Ok(())
}

async fn set_import_index_barrier_pending(
    conn: &DatabaseConnection,
    barrier: &ImportIndexBarrier,
    identity: Option<&ImportIdentityFence>,
) -> Result<()> {
    let txn = db::begin_write_transaction(conn)
        .await
        .context("begin import object-index partial finalization")?;
    if let Err(error) = barrier
        .capture_scope
        .assert_workspace_fence_live(&txn)
        .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before marking import object-index repair pending",
        );
    }
    lock_import_index_barrier_row(&txn, &barrier.session_id).await?;
    let mut marker = read_owned_import_index_barrier(&txn, barrier).await?;
    if let Some(identity) = identity {
        if identity.identity_id != marker.identity_id {
            anyhow::bail!("import object-index barrier identity changed before finalization");
        }
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_import_identity
             SET state = 'partial', last_error_code = 'LBR-AGENT-018', updated_at = ?
             WHERE identity_id = ? AND fence_token = ? AND state = 'committed'
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
                Utc::now().timestamp_millis().into(),
                identity.identity_id.clone().into(),
                identity.fence_token.into(),
                barrier.capture_scope.repo_id.clone().into(),
                barrier.capture_scope.worktree_id.clone().into(),
                barrier.capture_scope.workspace_id.clone().into(),
                barrier.capture_scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("mark exact import identity partial after object-index barrier failure")?;
        marker.fence_token = Some(identity.fence_token);
    }
    marker.state = "repair_pending".to_string();
    marker.lease_expires_at = 0;
    persist_import_index_barrier(&txn, &barrier.session_id, &marker).await?;
    if let Err(error) = barrier
        .capture_scope
        .assert_workspace_fence_live_for_commit(&txn)
        .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before committing import object-index partial finalization",
        );
    }
    txn.commit()
        .await
        .context("commit import object-index partial finalization")?;
    Ok(())
}

async fn clear_import_index_barrier(
    conn: &DatabaseConnection,
    barrier: &ImportIndexBarrier,
) -> Result<()> {
    let txn = db::begin_write_transaction(conn)
        .await
        .context("begin completed import object-index barrier retirement")?;
    if let Err(error) = barrier
        .capture_scope
        .assert_workspace_fence_live(&txn)
        .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before retiring import object-index barrier");
    }
    lock_import_index_barrier_row(&txn, &barrier.session_id).await?;
    read_owned_import_index_barrier(&txn, barrier).await?;
    MetadataKv::unset_with_conn(
        &txn,
        MetadataScope::AgentImportIndexRepair,
        &barrier.session_id,
        IMPORT_INDEX_REPAIR_MARKER_KEY,
    )
    .await
    .context("retire owned import object-index barrier marker")?;
    if let Err(error) = barrier
        .capture_scope
        .assert_workspace_fence_live_for_commit(&txn)
        .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before committing import object-index barrier retirement",
        );
    }
    txn.commit()
        .await
        .context("commit completed import object-index barrier retirement")?;
    Ok(())
}

async fn acquire_import_index_barrier(
    conn: &DatabaseConnection,
    storage_root: &Path,
    request: &ImportRequest,
    deadline: CaptureCommitDeadline,
) -> Result<ImportIndexBarrier> {
    // Do not wrap this transaction in an outer timeout: cancelling after
    // SQLx has dispatched COMMIT can report DeadlineExceeded while SQLite
    // subsequently makes the marker durable. The transaction's final SQL
    // authorization below owns the deadline decision; once it succeeds we
    // await COMMIT's acknowledgement unconditionally.
    let (barrier, needs_repair) =
        acquire_import_index_barrier_transaction(conn, request, deadline).await?;
    // Repairing an already-persisted marker is cleanup/recovery, rather than
    // a new V2 acquisition. Do not let the capture deadline suppress that
    // bounded handoff: leaving a provisional marker leased forever is worse
    // than recording it as repair-pending for a later scoped recovery.
    if needs_repair
        && let Err(error) =
            invoke_import_index_repair_helper(storage_root, &barrier, deadline.monotonic()).await
    {
        let pending = set_import_index_barrier_pending(conn, &barrier, None).await;
        return match pending {
            Ok(()) => Err(error),
            Err(pending_error) => Err(error.context(format!(
                "also failed to release the import object-index repair lease: {pending_error:#}"
            ))),
        };
    }
    Ok(barrier)
}

/// Acquire a new V2 import barrier under one deadline-bounded transaction.
///
/// The caller intentionally keeps recovery-helper work outside this future:
/// a deadline must roll back a new/overwritten marker, but it must not prevent
/// a best-effort transition of an already durable marker to repair-pending.
async fn acquire_import_index_barrier_transaction(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    deadline: CaptureCommitDeadline,
) -> Result<(ImportIndexBarrier, bool)> {
    let monotonic_deadline = deadline.monotonic();
    ensure_before_deadline(monotonic_deadline)?;
    if request.identity_schema_version != 2 || !is_import_source_commitment_v2(&request.source_id) {
        return Err(ImportError::SourceAuthorization.into());
    }
    let capture_scope = request.capture_scope.as_ref().context(
        "historical import request has no workspace scope; rerun `libra agent import` from the intended workspace",
    )?;
    let now_ms = Utc::now().timestamp_millis();
    let txn = tokio::time::timeout_at(
        tokio::time::Instant::from_std(monotonic_deadline),
        db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(ImportError::DeadlineExceeded))?
    .context("begin import object-index barrier acquisition")?;
    if let Err(error) = await_import_precommit_read_until(
        monotonic_deadline,
        capture_scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before acquiring import object-index barrier",
        );
    }
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify import deadline before locking import object-index barrier");
    }
    if let Err(error) = lock_import_index_barrier_row(&txn, &request.session_id).await {
        txn.rollback().await.ok();
        return Err(error);
    }
    let tombstone = match await_import_precommit_read_until(monotonic_deadline, async {
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
        .context("check import tombstone before object-index barrier acquisition")
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
    let existing = match await_import_precommit_read_until(monotonic_deadline, async {
        MetadataKv::get_with_conn(
            &txn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .context("read prior import object-index barrier marker")
    })
    .await
    {
        Ok(existing) => existing,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let needs_repair = existing.is_some();
    if let Some(entry) = existing.as_ref() {
        let prior = parse_import_index_barrier_marker(&entry.value)?;
        if prior.capture_scope.as_ref() != Some(capture_scope) {
            txn.rollback().await.ok();
            anyhow::bail!(
                "import object-index barrier belongs to a legacy or different workspace scope; run `libra agent doctor --repair` before retrying"
            );
        }
        if prior.state == "active" && prior.lease_expires_at > now_ms {
            txn.rollback().await.ok();
            return Err(ImportError::LeaseBusy.into());
        }
        // A V1 marker is migration evidence, not a stale record that a V2
        // writer may overwrite. The scoped migration must relabel its exact
        // expired proof atomically with identity/catalog ownership first.
        if prior.schema_version != IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2 {
            txn.rollback().await.ok();
            return Err(ImportError::RepositoryConflict.into());
        }
    }
    let marker = ImportIndexRepairMarker {
        schema_version: IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2,
        owner: format!(
            "index-barrier:{}:{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ),
        generation: uuid::Uuid::new_v4().to_string(),
        identity_id: import_identity_id(request),
        agent_kind: request.agent_kind.as_db_str().to_string(),
        provider_session_id: request.provider_session_id.clone(),
        source_kind: request.source_kind.clone(),
        source_id: request.source_id.clone(),
        state: "active".to_string(),
        lease_expires_at: now_ms
            .checked_add(IMPORT_INDEX_BARRIER_LEASE_MS)
            .context("import object-index barrier lease timestamp overflow")?,
        created_at: now_ms,
        fence_token: None,
        capture_scope: Some(capture_scope.clone()),
    };
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify import deadline before persisting import object-index barrier acquisition",
        );
    }
    if let Err(error) = persist_import_index_barrier(&txn, &request.session_id, &marker).await {
        txn.rollback().await.ok();
        return Err(error);
    }
    import_test_delay_after_index_barrier_persist();
    if let Err(error) = ensure_before_deadline(monotonic_deadline) {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify import deadline after persisting import object-index barrier acquisition",
        );
    }
    // This is the transaction's final SQL statement. It combines the
    // workspace fence and immutable SQLite wall-clock deadline, so a queued
    // writer that reaches SQLite after cutoff rolls every earlier mutation
    // back. No post-fence deadline check is allowed: successful
    // authorization linearizes the write before COMMIT acknowledgement.
    if let Err(error) =
        authorize_final_capture_commit(Some(capture_scope), &txn, Some(deadline)).await
    {
        txn.rollback().await.ok();
        return match error {
            CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                Err(anyhow::Error::from(ImportError::DeadlineExceeded))
            }
            error => Err(anyhow::Error::new(error)
                .context("authorize final import object-index barrier acquisition")),
        };
    }
    txn.commit()
        .await
        .context("commit import object-index barrier acquisition")?;
    let barrier = ImportIndexBarrier {
        session_id: request.session_id.clone(),
        marker,
        capture_scope: capture_scope.clone(),
    };
    Ok((barrier, needs_repair))
}

fn import_identity_fence(
    result: &anyhow::Result<DetailedImportSummary>,
) -> Option<ImportIdentityFence> {
    match result {
        Ok(detailed) => Some(ImportIdentityFence {
            identity_id: detailed.import_identity_id.clone(),
            fence_token: detailed.import_fence_token,
        }),
        Err(error) => error
            .downcast_ref::<ImportProgressError>()
            .map(ImportProgressError::detailed_summary)
            .map(|detailed| ImportIdentityFence {
                identity_id: detailed.import_identity_id,
                fence_token: detailed.import_fence_token,
            }),
    }
}

async fn import_index_barrier_erasure_won(
    conn: &DatabaseConnection,
    barrier: &ImportIndexBarrier,
) -> Result<bool> {
    #[cfg(test)]
    if test_support::fail_index_tombstone_lookup() {
        anyhow::bail!("test-only import index tombstone lookup failure");
    }
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
         WHERE agent_kind = ? AND provider_session_id = ?",
            [
                barrier.marker.agent_kind.clone().into(),
                barrier.marker.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check whether erasure owns import object-index barrier cleanup")?
        .is_some())
}

/// A command helper may only be spawned by the Libra executable registered
/// from `main`. Embedded library hosts intentionally have no fallback: their
/// own executable must never receive Libra's private helper argument.
fn require_registered_import_helper_command(
    command: Option<tokio::process::Command>,
) -> Result<tokio::process::Command> {
    command.ok_or_else(|| ImportError::AuthorizedReaderUnavailable.into())
}

/// Run the descriptor-owning preparation helper. `source` is an already
/// pinned provider descriptor (or anonymous export file); unlike the retired
/// V1 protocol, neither stdin nor the control frame carries a locator,
/// provider session id, repository path, storage path, or existing metadata.
#[cfg_attr(windows, allow(unused_mut))]
#[cfg_attr(windows, allow(dead_code))]
/// What the preparation helper reads as its standard input: a held source
/// file (an authorized transcript or the tempfile of export bytes), or a Pi
/// session that stays a `PiHeldSource` until the spawn.
enum PreparationInput {
    Held(std::fs::File),
    Pi(PiHeldSource),
}

impl From<std::fs::File> for PreparationInput {
    fn from(file: std::fs::File) -> Self {
        Self::Held(file)
    }
}

impl From<PiHeldSource> for PreparationInput {
    fn from(held: PiHeldSource) -> Self {
        Self::Pi(held)
    }
}

/// The helper's standard input, made at the spawn: a Pi source becomes it
/// directly, so the CLI never holds the Pi file as a `File`.
impl From<PreparationInput> for Stdio {
    fn from(input: PreparationInput) -> Self {
        match input {
            PreparationInput::Held(file) => Stdio::from(file),
            PreparationInput::Pi(held) => held.into_helper_stdin(),
        }
    }
}

async fn run_import_preparation_descriptor_helper_bounded(
    mut command: tokio::process::Command,
    control: PreparationDescriptorControl,
    source: impl Into<PreparationInput>,
    output_cap: u64,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let source = source.into();
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, control, source, output_cap, deadline);
        // Preparation can carry transcript-derived material. Without the
        // Unix dedicated-process-group contract a forked helper descendant
        // cannot be contained reliably, so this boundary fails closed.
        return Err(ImportError::AuthorizedReaderUnavailable.into());
    }

    #[cfg(any(unix, windows))]
    {
        ensure_before_deadline(deadline)?;
        let control =
            serde_json::to_string(&control).map_err(|_| ImportError::AuthorizedReaderFailed)?;
        if control.len() > 4 * 1024 {
            return Err(ImportError::AuthorizedReaderFailed.into());
        }
        command
            .env_clear()
            .current_dir(std::path::Path::new("/"))
            .env(IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV, control)
            .stdin(Stdio::from(source))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::internal::ai::authorized_read::configure_private_helper_process_group(&mut command);
        #[cfg(unix)]
        let child = command
            .spawn()
            .map_err(|_| ImportError::AuthorizedReaderFailed)?;
        #[cfg(unix)]
        let mut child = CancellationSafeChild::new_process_group(child);
        #[cfg(windows)]
        let mut child = CancellationSafeChild::spawn_windows(&mut command)
            .map_err(|_| ImportError::AuthorizedReaderFailed)?;
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            child.terminate_and_reap();
            return Err(ImportError::AuthorizedReaderFailed.into());
        };
        let mut stdout_task = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(delay) = test_support::preparation_response_read_delay() {
                tokio::time::sleep(delay).await;
            }
            read_async_strictly_bounded(&mut stdout, output_cap).await
        });
        child.register_abort_on_cancel(&stdout_task);

        // Drain stdout before reaping the leader. If a descendant inherited the
        // pipe after the leader exits, the deadline path still owns an unreaped
        // leader and can safely kill its process group before PID reuse.
        let response = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut stdout_task,
        )
        .await
        {
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                return Err(ImportError::AuthorizedReaderFailed.into());
            }
            Err(_) => {
                stdout_task.abort();
                child.terminate_and_reap();
                return Err(ImportError::DeadlineExceeded.into());
            }
        };
        if response.len() as u64 > output_cap {
            child.terminate_and_reap();
            return Err(ImportError::AuthorizedReaderFailed.into());
        }

        let status = match child.child_mut() {
            Some(child_process) => {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    child_process.wait(),
                )
                .await
                {
                    Ok(Ok(status)) => status,
                    Ok(Err(_)) => {
                        child.terminate_and_reap();
                        return Err(ImportError::AuthorizedReaderFailed.into());
                    }
                    Err(_) => {
                        child.terminate_and_reap();
                        return Err(ImportError::DeadlineExceeded.into());
                    }
                }
            }
            None => return Err(ImportError::AuthorizedReaderFailed.into()),
        };
        child.disarm_child_after_wait();
        if !status.success() || Instant::now() >= deadline {
            child.finish();
            return Err(if Instant::now() >= deadline {
                ImportError::DeadlineExceeded
            } else {
                ImportError::AuthorizedReaderFailed
            }
            .into());
        }
        child.finish();
        Ok(response)
    }
}

/// Consume the ordinary carrier only after the helper response's transport
/// checks. A damaged carrier preserves V1 projections and requests a retry.
#[cfg(any(unix, windows))]
fn consume_codex_retention(
    projection: &mut PreparedImportProjection,
    deadline: Instant,
) -> Option<crate::internal::ai::observed_agents::codex_reasoning::CodexRetention> {
    use crate::internal::ai::observed_agents::codex_reasoning::{
        CodexRetention, mark_retention_unavailable,
    };

    let retention = projection.codex_retention.take().map(|dto| {
        let source_bytes = projection
            .transcript_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.transcript_redacted_bytes);
        dto.decode(&projection.turns, source_bytes, deadline)
            .unwrap_or_else(|_| CodexRetention::unavailable())
    });
    if retention
        .as_ref()
        .is_some_and(CodexRetention::is_unavailable)
    {
        mark_retention_unavailable(&mut projection.redaction_report);
    }
    retention
}

async fn prepare_candidate_bounded(
    candidate: &Candidate,
    repo_root: &Path,
    storage_root: &Path,
    read_cap: u64,
    conn: &sea_orm::DatabaseConnection,
    capture_scope: &CaptureScope,
    deadline: CaptureCommitDeadline,
) -> Result<PreparedCandidateOutcome> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            candidate,
            repo_root,
            storage_root,
            read_cap,
            conn,
            capture_scope,
            deadline,
        );
        return Err(ImportError::AuthorizedReaderUnavailable.into());
    }

    #[cfg(any(unix, windows))]
    {
        let monotonic_deadline = deadline.monotonic();
        ensure_before_deadline(monotonic_deadline)?;
        if read_cap > TRANSCRIPT_READ_HARD_CAP_BYTES {
            return Err(ImportError::BatchInputLimit.into());
        }
        await_import_precommit_read_until(
            monotonic_deadline,
            capture_scope.assert_workspace_fence_live(conn),
        )
        .await
        .context("verify capture workspace lease before opening import source")?;
        // A Pi session file is opened, never read, here; the helper reads it.
        let (source, source_kind, source_id, pi_file) = if candidate.kind == AgentKind::Pi {
            let (file, source_id) = open_pi_session_file(candidate)?;
            (None, "file".to_string(), source_id, Some(file))
        } else {
            let (source, source_kind, source_id) =
                resolve_candidate_source(candidate, repo_root, monotonic_deadline).await?;
            (Some(source), source_kind, source_id, None)
        };
        import_test_pause_after_source_open(monotonic_deadline)?;
        let source_preimage = import_source_preimage(
            candidate.kind,
            &source_kind,
            &source_id,
            &candidate.provider_session_id,
        )?;
        let provisional_session_id = crate::internal::ai::hooks::runtime::build_ai_session_id(
            provider_name(candidate.kind),
            &candidate.provider_session_id,
        );
        let source = match source {
            None => match pi_file {
                Some(held) => PreparationInput::Pi(held),
                None => return Err(ImportError::AuthorizedReaderFailed.into()),
            },
            Some(TranscriptSource::File { file, .. }) => match file.into_rewound_inner() {
                Ok(file) => PreparationInput::Held(file),
                Err(_) => {
                    return Ok(PreparedCandidateOutcome {
                        request: Err(ImportError::AuthorizedReaderFailed.into()),
                        raw_bytes: 0,
                    });
                }
            },
            Some(TranscriptSource::Bytes { bytes, auth }) => {
                let raw_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if !auth.matches(candidate.kind.as_db_str(), &provisional_session_id, &bytes) {
                    return Ok(PreparedCandidateOutcome {
                        request: Err(ImportError::SourceAuthorization.into()),
                        raw_bytes,
                    });
                }
                if raw_bytes > read_cap {
                    return Ok(PreparedCandidateOutcome {
                        request: Err(ImportError::BatchInputLimit.into()),
                        raw_bytes,
                    });
                }
                let mut file = match tempfile::tempfile() {
                    Ok(file) => file,
                    Err(_) => {
                        return Ok(PreparedCandidateOutcome {
                            request: Err(ImportError::AuthorizedReaderFailed.into()),
                            raw_bytes,
                        });
                    }
                };
                if file.write_all(&bytes).is_err() || file.seek(SeekFrom::Start(0)).is_err() {
                    return Ok(PreparedCandidateOutcome {
                        request: Err(ImportError::AuthorizedReaderFailed.into()),
                        raw_bytes,
                    });
                }
                PreparationInput::Held(file)
            }
        };
        let remaining_ms = u64::try_from(
            monotonic_deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        if remaining_ms == 0 {
            return Err(ImportError::DeadlineExceeded.into());
        }
        let transport_key =
            ImportTransportKey::random().map_err(|_| ImportError::AuthorizedReaderFailed)?;
        let mut challenge = [0_u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut challenge)
            .map_err(|_| ImportError::AuthorizedReaderFailed)?;
        let control = PreparationDescriptorControl {
            agent_kind: candidate.kind.as_cli_slug().to_string(),
            source_kind: source_kind.clone(),
            provider_commitment: import_provider_commitment(&candidate.provider_session_id),
            // A Pi session past the Pi reader's cap would be refused anyway,
            // so the helper reads no more than that.
            read_cap: if candidate.kind == AgentKind::Pi {
                read_cap
                    .min(crate::internal::ai::observed_agents::builtin::pi::PI_NORMALIZE_MAX_BYTES)
            } else {
                read_cap
            },
            remaining_ms,
            transport: Some(PreparationImportTransportControl {
                version: 1,
                challenge,
                source_preimage_commitment: import_transport_source_preimage_commitment(
                    &source_preimage,
                ),
                scope_digest: import_transport_scope_digest(capture_scope)?,
                key: transport_key.encode_for_helper(),
            }),
        };
        let transport_binding = PreparationImportTransportBinding::from_control(&control)?
            .ok_or(ImportError::AuthorizedReaderFailed)?;
        let command = require_registered_import_helper_command(registered_helper_command(
            IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG,
        ))?;
        let response_bytes = run_import_preparation_descriptor_helper_bounded(
            command,
            control,
            source,
            IMPORT_PREPARATION_HELPER_OUTPUT_CAP,
            monotonic_deadline,
        )
        .await?;
        ensure_before_deadline(monotonic_deadline)?;
        let response: PreparationDescriptorHelperResponse = serde_json::from_slice(&response_bytes)
            .map_err(|_| ImportError::AuthorizedReaderFailed)?;
        match response {
            PreparationDescriptorHelperResponse::Ok {
                projection,
                raw_bytes,
                transport_required,
                transport,
            } => {
                let mut projection = *projection;
                if projection.storage_commitment != import_storage_commitment(storage_root)? {
                    return Ok(PreparedCandidateOutcome {
                        request: Err(ImportError::RepositoryConflict.into()),
                        raw_bytes,
                    });
                }
                // The descriptor helper may return its redacted-content
                // checksum only as a fixed-size transient preimage. Extract
                // it before this projection can enter any catalog/checkpoint
                // request, then replace it below with the scoped HMAC.
                let snapshot_preimage = projection
                    .transcript_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.redacted_digest_preimage())
                    .ok_or(ImportError::AuthorizedReaderFailed)?;
                // Authenticate the typed response while its helper projection
                // still carries the transient source checksum. A failed or
                // stale response must not reach key derivation or migration.
                let native_reasoning = consume_prepared_reasoning_transport(
                    transport_key,
                    &transport_binding,
                    candidate.kind,
                    &candidate.provider_session_id,
                    &projection,
                    transport_required,
                    transport,
                )?;
                validate_prepared_runtime_response(
                    candidate.kind,
                    &projection,
                    native_reasoning.is_some(),
                )?;
                // The native MAC above covers the entire received projection,
                // including this optional extension. Only now may it move.
                let coverage_runtime = match (candidate.kind, projection.coverage_runtime.take()) {
                    (AgentKind::OpenCode, Some(dto)) => {
                        let (turns, classifier_rejected, source_partial) =
                            decode_redacted_import_runtime(
                                dto,
                                candidate.kind.as_db_str(),
                                &projection.turns,
                                &projection.session_state,
                                monotonic_deadline,
                            )
                            .map_err(|_| ImportError::AuthorizedReaderFailed)?;
                        Some(
                            crate::internal::ai::capture::snapshot::CaptureCoverageRuntime {
                                turns,
                                classifier_rejected,
                                source_partial,
                            },
                        )
                    }
                    (AgentKind::OpenCode, None) | (_, Some(_)) => {
                        return Err(ImportError::AuthorizedReaderFailed.into());
                    }
                    (_, None) => None,
                };
                let codex_retention = consume_codex_retention(&mut projection, monotonic_deadline);
                await_import_precommit_read_until(
                    monotonic_deadline,
                    capture_scope.assert_workspace_fence_live(conn),
                )
                .await
                .context("verify capture workspace lease before deriving import source identity")?;
                let source_commitment = derive_capture_source_commitment_in_scope_until(
                    conn,
                    capture_scope,
                    storage_root,
                    repo_root,
                    CaptureSourceCommitmentDomain::ImportSourceV2,
                    &source_preimage,
                    monotonic_deadline,
                )
                .await
                .map_err(|error| import_commitment_failure(error, monotonic_deadline))?;
                let snapshot_commitment = derive_snapshot_content_commitment_in_scope_until(
                    conn,
                    capture_scope,
                    storage_root,
                    repo_root,
                    &snapshot_preimage,
                    monotonic_deadline,
                )
                .await
                .map_err(|error| import_commitment_failure(error, monotonic_deadline))?;
                if !projection
                    .transcript_snapshot
                    .as_mut()
                    .is_some_and(|snapshot| snapshot.bind_source_commitment(snapshot_commitment))
                {
                    return Err(ImportError::AuthorizedReaderFailed.into());
                }
                // History-only preflight permits a fresh session with no row.
                // Reservation rechecks rejected history under its first writer lock.
                if let Some(runtime) = &coverage_runtime {
                    await_import_precommit_read_until(
                        monotonic_deadline,
                        capture_scope.assert_provider_session_compatible(
                            conn,
                            &candidate.provider_session_id,
                        ),
                    )
                    .await
                    .context("verify scoped provider session before import history preflight")?;
                    crate::internal::ai::coverage_gate::ensure_runtime_import_source_history_until(
                        conn,
                        capture_scope,
                        &provisional_session_id,
                        deadline,
                        runtime.classifier_rejected,
                    )
                    .await?;
                }
                // A historical V1 row may still retain the exact raw locator
                // used to authorize this held descriptor. Move it only after
                // proving that legacy identity/catalog/(expired) barrier in
                // one scoped writer transaction; active or partial recovery
                // state remains V1 and blocks creation of a divergent V2 row.
                if let Err(error) = migrate_legacy_import_ownership_in_scope_until(
                    conn,
                    LegacyImportMigrationRequest {
                        scope: capture_scope,
                        storage_root,
                        authorized_root: repo_root,
                        agent_kind: candidate.kind,
                        provider_session_id: &candidate.provider_session_id,
                        source_kind: &source_kind,
                        legacy_source_id: &source_id,
                        v2_source_commitment: &source_commitment,
                        deadline,
                    },
                )
                .await
                {
                    // Keep the migration's stable failure contract. In
                    // particular, a tombstone or a competing lease must not
                    // be flattened into a misleading repository conflict.
                    if let Some(import_error) = error.downcast_ref::<ImportError>() {
                        return Err((*import_error).into());
                    }
                    return Err(ImportError::RepositoryConflict.into());
                }
                let mut request = import_request_from_projection(
                    candidate.kind,
                    candidate.provider_session_id.clone(),
                    source_kind,
                    source_commitment,
                    repo_root.to_path_buf(),
                    projection,
                );
                request.capture_scope = Some(capture_scope.clone());
                let existing_session = await_import_precommit_read_until(
                    monotonic_deadline,
                    load_existing_session_ownership_in_scope(
                        conn,
                        candidate.kind,
                        &candidate.provider_session_id,
                        capture_scope,
                    ),
                )
                .await?;
                validate_scoped_prepared_existing_session(&mut request, existing_session.as_ref())?;
                Ok(PreparedCandidateOutcome {
                    request: Ok(PreparedCandidate {
                        request,
                        native_reasoning,
                        coverage_runtime,
                        codex_retention,
                    }),
                    raw_bytes,
                })
            }
            PreparationDescriptorHelperResponse::Error {
                error_kind,
                raw_bytes,
            } => Ok(PreparedCandidateOutcome {
                request: Err(match error_kind {
                    Some(kind) => preparation_error(kind).into(),
                    None => anyhow::anyhow!("bounded import preparation rejected the source"),
                }),
                raw_bytes,
            }),
        }
    }
}

fn stable_code_for_error(error: &anyhow::Error) -> StableErrorCode {
    if error.is::<IndexCatalogUnavailable>()
        || error.is::<provider_index_io::ReconciliationReadFailure>()
    {
        return StableErrorCode::AgentImportPartialBatch;
    }
    match error.downcast_ref::<ImportError>() {
        Some(ImportError::RepositoryConflict | ImportError::SessionIdentityConflict) => {
            StableErrorCode::AgentImportRepositoryConflict
        }
        Some(ImportError::WorkingDirMissingOrAmbiguous) => {
            StableErrorCode::AgentImportWorkingDirInvalid
        }
        Some(ImportError::Erased) => StableErrorCode::AgentImportErased,
        Some(
            ImportError::SourceAuthorization
            | ImportError::AuthorizedReaderUnavailable
            | ImportError::AuthorizedReaderFailed,
        ) => StableErrorCode::AgentTranscriptAuthorizationMissing,
        Some(
            ImportError::LeaseBusy
            | ImportError::NoImportableTurns
            | ImportError::BatchInputLimit
            | ImportError::DeadlineExceeded
            | ImportError::FutureTimestamp,
        )
        | None => StableErrorCode::AgentImportPartialBatch,
    }
}

fn hashed_import_id(id: &str) -> String {
    let digest = sha2::Sha256::digest(id.as_bytes());
    format!("sha256:{}", hex::encode(&digest[..6]))
}

fn safe_failure_for(kind: AgentKind, id: &str, error: &anyhow::Error) -> BatchFailure {
    BatchFailure {
        status: "failed",
        agent_kind: kind.as_db_str().to_string(),
        session_id: hashed_import_id(id),
        error_code: stable_code_for_error(error),
        reason_key: None,
    }
}

fn safe_failure(candidate: &Candidate, error: &anyhow::Error) -> BatchFailure {
    safe_failure_for(candidate.kind, &candidate.provider_session_id, error)
}

fn safe_skip_for(kind: AgentKind, id: &str, error: &anyhow::Error) -> BatchSkip {
    let failure = safe_failure_for(kind, id, error);
    BatchSkip {
        status: "skipped",
        agent_kind: failure.agent_kind,
        session_id: failure.session_id,
        reason_code: failure.error_code,
        reason_key: None,
    }
}

fn safe_skip(candidate: &Candidate, error: &anyhow::Error) -> BatchSkip {
    safe_skip_for(candidate.kind, &candidate.provider_session_id, error)
}

fn is_discovery_skip(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ImportError>(),
        Some(ImportError::RepositoryConflict | ImportError::Erased)
    )
}

#[allow(clippy::too_many_arguments)]
fn record_candidate_result(
    candidate: &Candidate,
    discovered_batch: bool,
    index_incomplete: bool,
    result: anyhow::Result<DetailedImportSummary>,
    results: &mut Vec<BatchResult>,
    partial_results: &mut Vec<BatchResult>,
    skipped: &mut Vec<BatchSkip>,
    failures: &mut Vec<BatchFailure>,
) {
    match result {
        Ok(mut detailed) if index_incomplete => {
            detailed.summary.partial = true;
            partial_results.push(BatchResult::partial(detailed));
            failures.push(safe_failure(
                candidate,
                &ImportError::DeadlineExceeded.into(),
            ));
        }
        Ok(detailed) if detailed.summary.partial => {
            partial_results.push(BatchResult::partial(detailed));
            failures.push(safe_failure(candidate, &ImportError::LeaseBusy.into()));
        }
        Ok(detailed) => results.push(BatchResult::complete(detailed)),
        Err(error) => {
            if let Some(progress) = error.downcast_ref::<ImportProgressError>() {
                partial_results.push(BatchResult::partial(progress.detailed_summary()));
            }
            if index_incomplete {
                failures.push(safe_failure(
                    candidate,
                    &ImportError::DeadlineExceeded.into(),
                ));
            } else if discovered_batch && is_discovery_skip(&error) {
                skipped.push(safe_skip(candidate, &error));
            } else {
                failures.push(safe_failure(candidate, &error));
            }
        }
    }
}

struct IndexImportBatch {
    root: provider_index_io::ImportRootPin,
    rows: Vec<codex_index::ImportIndexThread>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "index import catalog state is unavailable; inspect the session with `libra agent doctor` before retrying"
)]
struct IndexCatalogUnavailable;

fn index_catalog_reason(end: Instant) -> &'static str {
    if Instant::now() >= end {
        "budget_exhausted"
    } else {
        "catalog_unavailable"
    }
}

fn index_import_unavailable(reason: &'static str) -> CliError {
    CliError::fatal("Codex index backfill is unavailable; supply a closed, consistent backup for this worktree and retry")
        .with_stable_code(StableErrorCode::AgentImportPartialBatch)
        .with_detail("reason_key",reason)
}

fn index_import_read_error(error: &anyhow::Error, end: Instant) -> CliError {
    let reason = error
        .downcast_ref::<provider_index_io::ReconciliationReadFailure>()
        .map(|reason| reason.reason_key())
        .or_else(|| {
            error
                .downcast_ref::<codex_index::IndexUnavailableReason>()
                .map(|reason| reason.reason_key())
        })
        .unwrap_or(if Instant::now() >= end {
            "budget_exhausted"
        } else {
            "backup_unreadable"
        });
    index_import_unavailable(reason)
}

async fn discover_index_import(
    args: &ImportArgs,
    root: &Path,
    end: Instant,
) -> CliResult<(IndexImportBatch, Option<usize>)> {
    if args.limit == 0 || args.limit > MAX_IMPORT_LIMIT {
        return Err(CliError::command_usage("--limit must be between 1 and 100")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if args
        .agent
        .as_deref()
        .map(importable_kind)
        .transpose()?
        .is_some_and(|kind| kind != AgentKind::Codex)
        || args.session.is_some()
        || args.path.is_some()
        || args.since.is_some()
        || args.all
        || args.restore_erased
    {
        return Err(CliError::command_usage("--codex-index-backup selects Codex only and cannot be combined with another selector or --restore-erased")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let backup = args
        .codex_index_backup
        .as_ref()
        .ok_or_else(|| index_import_unavailable("backup_unreadable"))?;
    let cwd = match crate::internal::worktree_scope::WorktreeScope::request_scope() {
        Some(request) => request.workdir,
        None => {
            std::env::current_dir().map_err(|_| index_import_unavailable("scope_root_unproven"))?
        }
    };
    let backup = util::normalize_abs_path(&if backup.is_absolute() {
        backup.clone()
    } else {
        cwd.join(backup)
    });
    let index_end = (Instant::now() + Duration::from_secs(2)).min(end);
    let read = provider_index_io::read_import_closed_backup(
        &backup,
        codex_index::ImportScopeQuery {
            version: 1,
            root: root.to_path_buf(),
            root_identity: None,
        },
        index_end,
    )
    .await
    .map_err(|error| index_import_read_error(&error, index_end))?;
    let snapshot = match read.outcome {
        ImportIndexOutcome::Available { snapshot } => snapshot,
        ImportIndexOutcome::Unavailable { reason } => {
            return Err(index_import_unavailable(reason.reason_key()));
        }
    };
    let offset = args.cursor.unwrap_or(0);
    if offset > snapshot.threads.len() {
        return Err(
            CliError::command_usage("--cursor is outside the discovery result set")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    let next = offset
        .saturating_add(args.limit)
        .min(snapshot.threads.len());
    let cursor = (next < snapshot.threads.len()).then_some(next);
    // Edges are verified projection-only metadata, never command/writer inputs.
    let rows = snapshot
        .threads
        .into_iter()
        .skip(offset)
        .take(args.limit)
        .collect();
    Ok((
        IndexImportBatch {
            root: read.root,
            rows,
        },
        cursor,
    ))
}

#[derive(Default)]
struct IndexSelection {
    candidates: Vec<Candidate>,
    existing: Vec<BatchExisting>,
    skipped: Vec<BatchSkip>,
    failures: Vec<BatchFailure>,
}

async fn select_index_missing(
    batch: &IndexImportBatch,
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    end: Instant,
) -> CliResult<IndexSelection> {
    batch
        .root
        .revalidate(end)
        .await
        .map_err(|error| index_import_read_error(&error, end))?;
    let ids = batch
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();
    let txn = await_import_precommit_read_until(end, async { Ok(conn.begin().await?) })
        .await
        .map_err(|_| index_import_unavailable(index_catalog_reason(end)))?;
    let now_ms = Utc::now().timestamp_millis();
    let mut metadata_remaining = 8 * 1024 * 1024;
    let states = await_import_precommit_read_until(end, async {
        Ok(CaptureCatalogStore::reconciliation_catalog_in_snapshot(
            &txn,
            scope,
            &ids,
            now_ms,
            end,
            &mut metadata_remaining,
        )
        .await?)
    })
    .await;
    // A cancelled read must finish its transaction before source authorization.
    txn.rollback()
        .await
        .map_err(|_| index_import_unavailable(index_catalog_reason(end)))?;
    let states = states.map_err(|_| index_import_unavailable(index_catalog_reason(end)))?;
    if states.len() != batch.rows.len() {
        return Err(index_import_unavailable("catalog_unavailable"));
    }
    batch
        .root
        .revalidate(end)
        .await
        .map_err(|error| index_import_read_error(&error, end))?;
    let mut selected = IndexSelection::default();
    for (row, state) in batch.rows.iter().zip(states) {
        match state {
            ReconciliationCatalogState::Erased => selected.skipped.push(safe_skip_for(
                AgentKind::Codex,
                &row.id,
                &ImportError::Erased.into(),
            )),
            ReconciliationCatalogState::Session { .. } => selected.existing.push(BatchExisting {
                status: "existing",
                agent_kind: AgentKind::Codex.as_db_str().to_string(),
                session_id: hashed_import_id(&row.id),
                selection_reason: "existing_catalog_session",
            }),
            ReconciliationCatalogState::Pending { .. } => selected.failures.push(safe_failure_for(
                AgentKind::Codex,
                &row.id,
                &ImportError::LeaseBusy.into(),
            )),
            ReconciliationCatalogState::Unknown => selected.failures.push(safe_failure_for(
                AgentKind::Codex,
                &row.id,
                &IndexCatalogUnavailable.into(),
            )),
            ReconciliationCatalogState::BudgetExhausted => {
                selected.failures.push(safe_failure_for(
                    AgentKind::Codex,
                    &row.id,
                    &provider_index_io::ReconciliationReadFailure::Budget.into(),
                ))
            }
            ReconciliationCatalogState::Missing => {
                if row.scope == RecentScopeStatus::Proven
                    && row.source_state == ImportSourceState::Eligible
                    && row
                        .rollout_path
                        .as_deref()
                        .is_some_and(|path| codex_index::valid_import_rollout_path(path, &row.id))
                {
                    selected.candidates.push(Candidate {
                        kind: AgentKind::Codex,
                        provider_session_id: row.id.clone(),
                        path: row.rollout_path.clone(),
                    });
                } else {
                    selected.failures.push(safe_failure_for(
                        AgentKind::Codex,
                        &row.id,
                        &ImportError::SourceAuthorization.into(),
                    ));
                }
            }
        }
    }
    Ok(selected)
}

fn index_preparation_error(error: anyhow::Error, index_mode: bool) -> anyhow::Error {
    if index_mode
        && matches!(
            error.downcast_ref::<ImportError>(),
            Some(
                ImportError::RepositoryConflict
                    | ImportError::SessionIdentityConflict
                    | ImportError::WorkingDirMissingOrAmbiguous
            )
        )
    {
        ImportError::SourceAuthorization.into()
    } else {
        error
    }
}

pub async fn execute_safe(args: ImportArgs, output: &OutputConfig) -> CliResult<()> {
    // Establish both deadline clocks once before discovery/consent or any
    // mutation.  The pair is threaded unchanged to every normal durable
    // import write; only read/helper operations consume its monotonic half.
    let commit_deadline = CaptureCommitDeadline::from_budget(import_total_deadline())
        .map_err(|error| CliError::fatal(format!("establish import deadline: {error}")))?;
    let deadline = commit_deadline.monotonic();
    let (mut repo_root, storage_root) =
        match crate::internal::worktree_scope::WorktreeScope::request_scope() {
            Some(request) => (request.worktree_root, request.storage),
            None => (
                util::try_working_dir().map_err(|_| CliError::repo_not_found())?,
                util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?,
            ),
        };
    let index_mode = args.codex_index_backup.is_some();
    // Batch Codex discovery reads a bounded session_meta record to apply
    // --since. Get interactive consent before that read; --yes is itself the
    // explicit consent on noninteractive paths. The exact candidate count is
    // unavailable until discovery finishes.
    let preconsented = !args.yes && (args.all || args.since.is_some());
    if preconsented {
        let _ = validate_discovery_selector(&args)?;
        require_consent(&args, output, None, deadline)?;
    }
    let mut index_batch = None;
    let (mut candidates, next_cursor, discovery_rejected) = if index_mode {
        let (batch, cursor) = discover_index_import(&args, &repo_root, deadline).await?;
        repo_root = batch.root.path().to_path_buf();
        index_batch = Some(batch);
        (Vec::new(), cursor, Vec::new())
    } else {
        discover_bounded(&args, &repo_root, deadline).await?
    };
    let selection_count = index_batch
        .as_ref()
        .map_or(candidates.len(), |batch| batch.rows.len());
    if !preconsented {
        require_consent(&args, output, Some(selection_count), deadline)?;
    }
    if let Some(batch) = index_batch.as_ref() {
        batch
            .root
            .revalidate(deadline)
            .await
            .map_err(|error| index_import_read_error(&error, deadline))?;
    }
    let conn = db::get_db_conn_instance_for_path(&storage_root.join(util::DATABASE))
        .await
        .map_err(|error| CliError::fatal(format!("failed to open repository database: {error}")))?;
    let capture_scope =
        await_import_precommit_read_until(deadline, CaptureScope::resolve(&conn, &repo_root))
            .await
            .map_err(|error| {
                if index_mode {
                    index_import_unavailable("scope_root_unproven")
                } else {
                    CliError::fatal(format!(
                        "failed to resolve import workspace scope: {error:#}"
                    ))
                }
            })?;
    let selected = if let Some(batch) = index_batch.as_ref() {
        select_index_missing(batch, &conn, &capture_scope, deadline).await?
    } else {
        IndexSelection::default()
    };
    if index_mode {
        candidates = selected.candidates;
    }
    let (configured_source_cap, explicitly_configured) =
        max_transcript_read_bytes_setting().await.map_err(|error| {
            CliError::fatal(format!(
                "failed to read transcript input limit config: {error:#}"
            ))
        })?;
    let source_read_cap = effective_source_read_cap(configured_source_cap);
    if explicitly_configured && configured_source_cap > TRANSCRIPT_READ_HARD_CAP_BYTES {
        eprintln!(
            "note: config '{MAX_TRANSCRIPT_READ_BYTES_KEY}' is {configured_source_cap} bytes, but historical import's adapter hard cap is {TRANSCRIPT_READ_HARD_CAP_BYTES} bytes; effective per-source cap is {source_read_cap} bytes"
        );
    }

    let mut results = Vec::new();
    let mut partial_results = Vec::new();
    let mut skipped = selected.skipped;
    let mut failures = selected.failures;
    let mut discovery_skipped = 0usize;
    for (session_id, reason) in discovery_rejected {
        let tombstoned = await_import_precommit_read_until(
            deadline,
            session_is_tombstoned(&conn, AgentKind::Codex, &session_id),
        )
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to check rejected Codex session tombstone: {error}"
            ))
        })?;
        if tombstoned && !args.restore_erased {
            skipped.push(safe_skip_for(
                AgentKind::Codex,
                &session_id,
                &ImportError::Erased.into(),
            ));
            continue;
        }
        skipped.push(BatchSkip {
            status: "skipped",
            agent_kind: AgentKind::Codex.as_db_str().to_owned(),
            session_id: hashed_import_id(&session_id),
            reason_code: StableErrorCode::AgentTranscriptAuthorizationMissing,
            reason_key: Some(match reason {
                DiscoveryRejection::CodexUnknownSessionTime => "codex_session_time_unknown",
                DiscoveryRejection::CodexSourceUnreadable => "codex_session_source_unreadable",
                _ => "codex_session_header_invalid",
            }),
        });
        discovery_skipped += 1;
    }
    if discovery_skipped > 0 {
        let message = format!(
            "Codex --since skipped {discovery_skipped} rollout(s) with unknown time, invalid header, or unreadable source; rerun with --json for hashed IDs and reason keys, then inspect and import selected sessions with --session or --path"
        );
        crate::utils::output::record_warning_message(message.clone());
        if !output.quiet && !output.is_json() {
            eprintln!("warning: {message}");
        }
    }
    let existing = selected.existing;
    let discovered_batch = args.all || args.since.is_some() || index_mode;
    let mut cumulative_raw_bytes = 0_u64;
    let raw_byte_cap = batch_raw_byte_cap();
    let mut index_terminal_reason = None;
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        if Instant::now() >= deadline {
            failures.push(safe_failure(
                candidate,
                &ImportError::DeadlineExceeded.into(),
            ));
            continue;
        }
        let tombstoned = await_import_precommit_read_until(
            deadline,
            session_is_tombstoned(&conn, candidate.kind, &candidate.provider_session_id),
        )
        .await;
        let tombstoned = match tombstoned {
            Ok(value) => value,
            Err(error) if index_mode => {
                failures.push(safe_failure(candidate, &error));
                index_terminal_reason = Some(index_catalog_reason(deadline));
                continue;
            }
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "failed to check import tombstone: {error}"
                )));
            }
        };
        if tombstoned && args.restore_erased {
            restore_tombstone(&conn, candidate.kind, &candidate.provider_session_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "failed to restore erased provider session: {error}"
                    ))
                })?;
        } else if tombstoned {
            let error = anyhow::Error::from(ImportError::Erased);
            if discovered_batch {
                skipped.push(safe_skip(candidate, &error));
            } else {
                failures.push(safe_failure(candidate, &error));
            }
            continue;
        }

        let index_failures_before = ClientStorage::background_index_failure_count();
        let mut index_barrier = None;
        let mut result = async {
            ensure_before_deadline(deadline)?;
            let remaining_raw_bytes = raw_byte_cap
                .checked_sub(cumulative_raw_bytes)
                .ok_or(ImportError::BatchInputLimit)?
                .min(source_read_cap);
            if let Some(batch)=index_batch.as_ref() {batch.root.revalidate(deadline).await?;}
            let prepared = prepare_candidate_bounded(
                candidate,
                &repo_root,
                &storage_root,
                remaining_raw_bytes,
                &conn,
                &capture_scope,
                commit_deadline,
            )
            .await;
            // A concurrent root replacement can itself make preparation fail.
            // Recheck even on that error before classifying it as source-only.
            if let Some(batch)=index_batch.as_ref() {batch.root.revalidate(deadline).await?;}
            let prepared = prepared.map_err(|error|index_preparation_error(error,index_mode))?;
            cumulative_raw_bytes = cumulative_raw_bytes
                .checked_add(prepared.raw_bytes)
                .ok_or(ImportError::BatchInputLimit)?;
            if cumulative_raw_bytes > raw_byte_cap {
                return Err(ImportError::BatchInputLimit.into());
            }
            let PreparedCandidate { request, native_reasoning, coverage_runtime, codex_retention } = prepared.request.map_err(|error|index_preparation_error(error,index_mode))?;
            let barrier = acquire_import_index_barrier(
                &conn,
                &storage_root,
                &request,
                commit_deadline,
            )
            .await?;
            index_barrier = Some(barrier);
            import_test_pause_after_index_barrier(deadline)?;
            let child_binding = LiveCaptureBinding::for_stored(request.agent_kind);
            let subagent_discovery = if let Some(port) = child_binding.subagent_discovery() {
                let candidate_allowance =
                    remaining_candidate_read_allowance(source_read_cap, prepared.raw_bytes);
                let subagent_budget = raw_byte_cap
                    .checked_sub(cumulative_raw_bytes)
                    .ok_or(ImportError::BatchInputLimit)?
                    .min(candidate_allowance);
                // Reserve the whole allowance before reading so a failed
                // discovery remains charged and repeated malformed sources
                // cannot bypass the batch cap. On success refund unused bytes.
                reserve_subagent_input_allowance(&mut cumulative_raw_bytes, subagent_budget)?;
                let discovery_deadline = crate::internal::ai::subagent_content::discovery_deadline_preserving_parent(deadline)?;
                let context = LiveCaptureContext {
                    libra_session_id: &request.session_id, provider_session_id: &request.provider_session_id,
                    verified_cwd: &request.working_dir,
                };
                let discovery_result = port.discover_stored(
                    &context,
                    StoredSubagentSelection::ImportCurrent { parent_locator: candidate.path.as_deref().filter(|path| path.is_absolute()) },
                    SubagentDiscoveryLimits { byte_budget: subagent_budget, source_limit: crate::internal::ai::subagent_content::MAX_SUBAGENT_SOURCES_PER_CAPTURE },
                    discovery_deadline,
                ).await;
                match discovery_result {
                    Ok(discovery) => {
                        settle_subagent_input_allowance(
                            &mut cumulative_raw_bytes,
                            subagent_budget,
                            discovery.bytes_read,
                        )?;
                        discovery
                    }
                    Err(_) if request.agent_kind == AgentKind::Codex => crate::internal::ai::subagent_content::SubagentDiscovery::charged_stored_unavailable(subagent_budget),
                    Err(error) => match crate::internal::ai::subagent_content::SubagentDiscovery::from_deadline_error(&error) {
                        Some(discovery) => discovery,
                        None => return Err(error),
                    },
                }
            } else {
                crate::internal::ai::subagent_content::SubagentDiscovery::default()
            };
            import_prepared_with_retention(
                &conn,
                &storage_root,
                request,
                commit_deadline,
                subagent_discovery,
                native_reasoning,
                coverage_runtime,
                codex_retention,
            )
            .await
        }
        .await;
        // Do not advertise a candidate as complete until every object-index
        // write it enqueued is visible. The durable marker is installed before
        // persistence starts, so timeout, terminal index errors, and process
        // crashes all force foreground repair before replay can become noop.
        let index_drained = ClientStorage::wait_for_background_tasks_until(deadline).await;
        let index_failed = ClientStorage::background_index_failure_count() != index_failures_before;
        let mut index_incomplete = !index_drained || index_failed;
        if let Some(barrier) = index_barrier.as_ref() {
            let identity = import_identity_fence(&result);
            let result_is_erased = matches!(
                result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<ImportError>()),
                Some(ImportError::Erased)
            );
            // A tombstone-confirmed erasure owns marker/index cleanup. Do not
            // turn its actionable LBR-AGENT-019 result into a generic index
            // partial merely because the erasure transaction removed our
            // barrier generation before this process could retire it.
            match import_index_barrier_erasure_won(&conn, barrier).await {
                Ok(true) => {
                    index_incomplete = false;
                    result = Err(ImportError::Erased.into());
                }
                Ok(false) if result_is_erased => {
                    // The writer already observed the tombstone in a fenced
                    // transaction. Preserve that result even if a concurrent
                    // cleanup/restore makes this advisory recheck return false.
                    index_incomplete = false;
                }
                Err(error) if result_is_erased => {
                    // A diagnostic recheck must never hide the stronger,
                    // already-established erasure result.
                    tracing::error!(
                        error = %error,
                        "failed to recheck import tombstone after the writer was already fenced by erasure"
                    );
                    index_incomplete = false;
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "failed to determine whether erasure owns import object-index barrier cleanup"
                    );
                    // Fail closed: a successful writer cannot be advertised as
                    // complete until the marker is safely retired. If the
                    // candidate already has a distinct failure, preserve that
                    // failure instead of replacing it with the advisory lookup
                    // error; the durable marker still drives replay repair.
                    index_incomplete = result.is_ok();
                    if let Err(mark_error) =
                        set_import_index_barrier_pending(&conn, barrier, identity.as_ref()).await
                    {
                        tracing::error!(
                            error = %mark_error,
                            "failed to preserve import object-index repair ownership after tombstone lookup failure"
                        );
                    }
                }
                Ok(false) if index_incomplete => {
                    if let Err(error) =
                        set_import_index_barrier_pending(&conn, barrier, identity.as_ref()).await
                    {
                        tracing::error!(
                            error = %error,
                            "failed to preserve import object-index repair ownership after barrier failure"
                        );
                    }
                }
                Ok(false) => {
                    if let Err(error) = clear_import_index_barrier(&conn, barrier).await {
                        tracing::error!(
                            error = %error,
                            "failed to retire completed import object-index barrier"
                        );
                        index_incomplete = true;
                        if let Err(mark_error) =
                            set_import_index_barrier_pending(&conn, barrier, identity.as_ref())
                                .await
                        {
                            tracing::error!(
                                error = %mark_error,
                                "failed to preserve import object-index repair ownership after retirement failure"
                            );
                        }
                    }
                }
            }
        }
        let root_failure = if index_mode {
            result
                .as_ref()
                .err()
                .and_then(|error| {
                    error.downcast_ref::<provider_index_io::ReconciliationReadFailure>()
                })
                .copied()
        } else {
            None
        };
        record_candidate_result(
            candidate,
            discovered_batch && !index_mode,
            index_incomplete,
            result,
            &mut results,
            &mut partial_results,
            &mut skipped,
            &mut failures,
        );
        if let Some(reason) = root_failure {
            index_terminal_reason = Some(reason.reason_key());
            // Keep every unattempted slot visible without reading a source
            // after its directory proof has failed. Earlier writes remain.
            for remaining in &candidates[candidate_index + 1..] {
                failures.push(safe_failure(remaining, &reason.into()));
            }
            break;
        }
    }

    let payload = BatchOutput {
        schema_version: 1,
        results,
        partial_results,
        skipped,
        failures,
        next_cursor,
        existing,
        warnings: crate::utils::output::pending_warning_messages(),
    };
    if !payload.failures.is_empty() {
        let stable_code = if selection_count == 1
            && payload.results.is_empty()
            && payload.partial_results.is_empty()
            && payload.failures.len() == 1
        {
            payload.failures[0].error_code
        } else {
            StableErrorCode::AgentImportPartialBatch
        };
        let mut message = if selection_count == 1
            && payload.results.is_empty()
            && payload.partial_results.is_empty()
        {
            "agent import failed; resolve the reported stable error code and retry (run `libra agent doctor --repair` if object-index repair cannot complete)".to_string()
        } else {
            format!(
                "agent import completed partially: {} succeeded, {} made partial progress, {} failed; rerun failed selections after resolving the reported stable error codes (or run `libra agent doctor --repair` if object-index repair cannot complete)",
                payload.results.len(),
                payload.partial_results.len(),
                payload.failures.len()
            )
        };
        if !payload.existing.is_empty() {
            message.push_str(&format!(
                "; {} already cataloged (not refreshed)",
                payload.existing.len()
            ));
        }
        let mut error = CliError::fatal(message)
            .with_stable_code(stable_code)
            .with_detail("schema_version", payload.schema_version)
            .with_detail("succeeded", payload.results.len())
            .with_detail("partial", payload.partial_results.len())
            .with_detail("skipped", payload.skipped.len())
            .with_detail("failed", payload.failures.len())
            .with_detail("next_cursor", payload.next_cursor);
        if let Some(reason) = index_terminal_reason {
            error = error.with_detail("reason_key", reason);
        }
        if let Ok(failures) = serde_json::to_value(&payload.failures) {
            error = error.with_detail("failures", failures);
        }
        if let Ok(results) = serde_json::to_value(&payload.results) {
            error = error.with_detail("results", results);
        }
        if let Ok(partial_results) = serde_json::to_value(&payload.partial_results) {
            error = error.with_detail("partial_results", partial_results);
        }
        if let Ok(skipped) = serde_json::to_value(&payload.skipped) {
            error = error.with_detail("skipped", skipped);
        }
        if !payload.existing.is_empty() {
            let existing=payload.existing.iter().map(|item|serde_json::json!({
                "status":item.status,"agent_kind":item.agent_kind,"session_id":item.session_id,"selection_reason":item.selection_reason,
            })).collect::<Vec<_>>();
            error = error.with_detail("existing", serde_json::Value::Array(existing));
        }
        if !payload.warnings.is_empty() {
            error = error.with_detail("warnings", payload.warnings.clone());
        }
        return Err(error);
    }
    if output.is_json() {
        return emit_json_data("agent_import", &payload, output);
    }
    if !output.quiet {
        println!(
            "Imported {} session(s), skipped {} session(s), {} turn checkpoint(s), {} subagent checkpoint(s); next cursor: {}",
            payload.results.len(),
            payload.skipped.len(),
            payload
                .results
                .iter()
                .map(|result| result.summary.checkpoints_written)
                .sum::<usize>(),
            payload
                .results
                .iter()
                .map(|result| result.subagent_checkpoints_written)
                .sum::<usize>(),
            payload
                .next_cursor
                .map(|cursor| cursor.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
    }
    if !output.quiet && !payload.existing.is_empty() {
        println!(
            "Existing catalog sessions (not refreshed): {}",
            payload.existing.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::internal::ai::{
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotProjection, CaptureSnapshotSource,
            CaptureSnapshotSourceKind,
        },
        observed_agents::reasoning::{
            ReasoningSourceDisposition, ReasoningTransportError,
            invalidate_import_transport_native_proof_for_test, synthetic_native_preparation,
        },
    };
    use crate::utils::error::CliErrorKind;

    /// PI-29: `agent import --agent` accepts exactly the prepared-import kind
    /// set, through every slug form, and its error names that same set.
    #[test]
    fn import_kind_gate_cli_accepts_exactly_the_set() {
        use crate::internal::ai::agent_import::{IMPORTABLE_AGENT_KINDS, is_importable_kind};

        for kind in AgentKind::all() {
            assert_eq!(
                importable_kind_from_slug(kind.as_cli_slug()),
                is_importable_kind(*kind).then_some(*kind),
                "{kind:?}"
            );
            assert_eq!(
                importable_kind_from_slug(kind.as_db_str()),
                is_importable_kind(*kind).then_some(*kind),
                "{kind:?}"
            );
        }
        let refused = importable_kind("grok").expect_err("grok is not importable");
        let message = refused.to_string();
        assert!(
            message.contains(
                "agent import supports claude-code, codex, opencode, hermes, or pi; got 'grok'"
            ),
            "{message}"
        );
        for kind in IMPORTABLE_AGENT_KINDS {
            assert!(message.contains(kind.as_cli_slug()), "{kind:?}: {message}");
        }
    }

    #[cfg(feature = "test-live-agent")]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_discovery_oversize_header_charges_cumulative_budget() {
        use std::io::{Seek, SeekFrom, Write};

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![b'x'; (CODEX_DISCOVERY_HEADER_CAP + 1) as usize])
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut charged = CODEX_DISCOVERY_HEADER_TOTAL_CAP - CODEX_DISCOVERY_HEADER_CAP;
        let error = codex_discovery_session_time(file, "test-session", &mut charged).unwrap_err();
        assert!(error.is::<CodexDiscoveryBudgetExceeded>());
        assert_eq!(
            charged,
            CODEX_DISCOVERY_HEADER_TOTAL_CAP + 1,
            "oversize bytes must be charged before the per-file rejection"
        );
    }

    #[cfg(feature = "test-live-agent")]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_discovery_budget_error_is_actionable() {
        for cause in [
            anyhow::Error::from(CodexDiscoveryBudgetExceeded),
            anyhow::Error::from(CodexDiscoveryEntryBudgetExceeded),
            anyhow::Error::from(CodexRolloutScanLimit("test bound".to_string())),
        ] {
            let reason = DiscoveryRejection::from_provider_error(
                &cause,
                DiscoveryRejection::CodexProviderRoot,
            );
            assert_eq!(reason, DiscoveryRejection::CodexDiscoveryBudgetExceeded);
            let error = reason.into_cli_error();
            assert_eq!(
                error.stable_code(),
                StableErrorCode::AgentImportPartialBatch
            );
            assert!(error.message().contains("4096 unfilterable-record"));
            assert!(error.message().contains("--path"));
            assert!(error.message().contains("index backup"));
        }
    }

    #[cfg(feature = "test-live-agent")]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_discovery_rejection_cap_fails_before_an_unreported_skip() {
        let mut rejected = Vec::new();
        for index in 0..CODEX_DISCOVERY_REJECTION_CAP {
            record_codex_discovery_rejection(
                &mut rejected,
                &format!("session-{index}"),
                DiscoveryRejection::CodexUnknownSessionTime,
            )
            .unwrap();
        }
        let error = record_codex_discovery_rejection(
            &mut rejected,
            "overflow",
            DiscoveryRejection::CodexUnknownSessionTime,
        )
        .unwrap_err();
        assert!(error.is::<CodexDiscoveryBudgetExceeded>());
        assert_eq!(rejected.len(), CODEX_DISCOVERY_REJECTION_CAP);
        let reason =
            DiscoveryRejection::from_provider_error(&error, DiscoveryRejection::CodexProviderRoot);
        assert_eq!(
            reason.into_cli_error().stable_code(),
            StableErrorCode::AgentImportPartialBatch
        );
    }

    #[cfg(all(unix, feature = "test-live-agent"))]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_discovery_read_error_is_not_called_an_invalid_header() {
        let directory = tempfile::tempdir().unwrap();
        let file = std::fs::File::open(directory.path()).unwrap();
        let mut header_bytes = 0;
        let mut rejected = Vec::new();
        let include = codex_discovery_filter(
            file,
            "local-session",
            Some(0),
            &mut header_bytes,
            &mut rejected,
        )
        .unwrap();
        assert!(!include);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].1, DiscoveryRejection::CodexSourceUnreadable);
    }

    #[cfg(unix)]
    #[cfg(feature = "test-live-agent")]
    mod codex_retention_local {
        use std::str::FromStr;

        use git_internal::hash::ObjectHash;
        use serde_json::{Value, json};

        use super::*;

        #[test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        fn codex_index_full_scope_unavailable_errors_keep_018_and_actionable_display() {
            assert_eq!(
                IndexCatalogUnavailable.to_string(),
                "index import catalog state is unavailable; inspect the session with `libra agent doctor` before retrying"
            );
            assert_eq!(
                stable_code_for_error(&IndexCatalogUnavailable.into()),
                StableErrorCode::AgentImportPartialBatch
            );
            for reason in [
                provider_index_io::ReconciliationReadFailure::ScopeRootUnproven,
                provider_index_io::ReconciliationReadFailure::ResponseInconsistent,
                provider_index_io::ReconciliationReadFailure::Budget,
            ] {
                assert_eq!(
                    stable_code_for_error(&reason.into()),
                    StableErrorCode::AgentImportPartialBatch
                );
            }
            assert_eq!(index_catalog_reason(Instant::now()), "budget_exhausted");
            assert_eq!(
                index_catalog_reason(Instant::now() + Duration::from_secs(1)),
                "catalog_unavailable"
            );
        }

        fn run_cli(
            program: &Path,
            root: &Path,
            home: &Path,
            args: &[&str],
        ) -> std::process::Output {
            std::process::Command::new(program)
                .args(args)
                .current_dir(root)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("USERPROFILE", home)
                .env("LIBRA_TEST_HOME", home)
                .env("LIBRA_TEST", "1")
                .output()
                .unwrap()
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_index_full_scope_invalid_auth_path_never_dispatches_source_read() {
            use std::sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            };

            use base64::Engine;
            use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};

            use crate::internal::{
                ai::authorized_read,
                worktree_scope::{RequestScope, with_request_scope},
            };

            async fn invoke(
                program: &Path,
                root: &Path,
                backup: &Path,
                calls: Arc<AtomicUsize>,
            ) -> CliResult<()> {
                let mut args = selector_args(|_| {});
                args.all = false;
                args.agent = None;
                args.yes = true;
                args.codex_index_backup = Some(backup.to_path_buf());
                let output = OutputConfig {
                    quiet: true,
                    ..OutputConfig::default()
                };
                with_request_scope(
                    Some(RequestScope::resolve(root.to_path_buf()).unwrap()),
                    INDEX_SOURCE_READS.scope(
                        calls,
                        authorized_read::with_test_helper_program(
                            program.to_path_buf(),
                            execute_safe(args, &output),
                        ),
                    ),
                )
                .await
            }
            async fn set_path(backup: &Path, path: &Path) {
                let mut connection =
                    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(backup))
                        .await
                        .unwrap();
                sea_orm::sqlx::query("UPDATE threads SET rollout_path=?")
                    .bind(path.to_str().unwrap())
                    .execute(&mut connection)
                    .await
                    .unwrap();
                connection.close().await.unwrap();
            }

            let program = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("libra");
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("repo");
            let home = temp.path().join("home");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&home).unwrap();
            let root = root.canonicalize().unwrap();
            let init = run_cli(&program, &root, &home, &["init"]);
            assert!(init.status.success());
            let backup = temp.path().join("index.sqlite");
            let mut connection = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&backup)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
            for sql in [
                "CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY,success INTEGER NOT NULL,checksum BLOB NOT NULL)",
                "CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT NOT NULL,cwd TEXT NOT NULL,archived INTEGER NOT NULL,cli_version TEXT NOT NULL)",
                "CREATE TABLE thread_spawn_edges(parent_thread_id TEXT NOT NULL,child_thread_id TEXT PRIMARY KEY,status TEXT NOT NULL)",
            ] {
                sea_orm::sqlx::query(sql)
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            for (version, digest) in codex_index::required_migrations_for_local_test() {
                sea_orm::sqlx::query("INSERT INTO _sqlx_migrations VALUES(?,1,?)")
                    .bind(version)
                    .bind(hex::decode(digest).unwrap())
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            // A rollout-shaped locator outside the configured provider root
            // reaches authorization, proving that this task-local observer works.
            let control = temp.path().join("rollout-control-counter-control.jsonl");
            std::fs::write(&control, b"non-sensitive positive-control fixture").unwrap();
            sea_orm::sqlx::query("INSERT INTO threads VALUES('counter-control',?,?,0,'0.162.1')")
                .bind(control.to_str().unwrap())
                .bind(root.to_str().unwrap())
                .execute(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let result = invoke(&program, &root, &backup, calls.clone()).await;
            assert!(result.is_err());
            assert!(
                calls.load(Ordering::SeqCst) >= 1,
                "eligible Missing positive control must reach the actual reader entry"
            );

            let mut paths = Vec::new();
            for leaf in [
                "auth.json",
                "config.toml",
                "history.jsonl",
                "state_5.sqlite",
            ] {
                let path = temp.path().join(leaf);
                std::fs::write(&path, b"CX19-ZERO-READ-SENTINEL").unwrap();
                paths.push(path);
            }
            paths.extend([
                PathBuf::from("auth.json"),
                temp.path().join("..").join("auth.json"),
                temp.path().join("rollout-control-wrong-sid.jsonl"),
            ]);
            for path in paths {
                set_path(&backup, &path).await;
                calls.store(0, Ordering::SeqCst);
                let error = invoke(&program, &root, &backup, calls.clone())
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.stable_code(),
                    StableErrorCode::AgentTranscriptAuthorizationMissing
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    0,
                    "invalid-shaped path reached source authorization"
                );
            }
            let forged = codex_index::ImportIndexOutcome::Available {
                snapshot: codex_index::ImportIndexSnapshot {
                    max_migration: 21,
                    candidate_count: 1,
                    scope_ambiguous_count: 0,
                    scope_unknown_count: 0,
                    threads: vec![codex_index::ImportIndexThread {
                        id: "counter-control".into(),
                        archived: false,
                        scope: RecentScopeStatus::Proven,
                        source_state: ImportSourceState::Eligible,
                        rollout_path: Some(temp.path().join("auth.json")),
                    }],
                    spawn_edges: Vec::new(),
                },
            };
            let frame = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&forged).unwrap());
            let fake = temp.path().join("forged-frame-helper");
            std::fs::write(&fake,format!("#!/usr/bin/python3\nimport base64,sys\nsys.stdout.buffer.write(base64.b64decode('{frame}'))\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
            calls.store(0, Ordering::SeqCst);
            let error = invoke(&fake, &root, &backup, calls.clone())
                .await
                .unwrap_err();
            assert_eq!(
                error.stable_code(),
                StableErrorCode::AgentImportPartialBatch
            );
            assert_eq!(
                serde_json::from_str::<Value>(&error.render_json()).unwrap()["details"]["reason_key"],
                "response_inconsistent"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let conn = sea_orm::Database::connect(format!(
                "sqlite://{}?mode=rw",
                root.join(util::ROOT_DIR).join(util::DATABASE).display()
            ))
            .await
            .unwrap();
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_session",
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
            conn.close().await.unwrap();
        }

        async fn helper_projection(
            program: &Path,
            source: &[u8],
            session: &str,
            fault: u8,
        ) -> PreparedImportProjection {
            let mut descriptor = tempfile::tempfile().unwrap();
            descriptor.write_all(source).unwrap();
            descriptor.rewind().unwrap();
            let mut command = tokio::process::Command::new(program);
            command.arg(IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG);
            let bytes = run_import_preparation_descriptor_helper_bounded(
                command,
                PreparationDescriptorControl {
                    agent_kind: "codex".to_owned(),
                    source_kind: "file".to_owned(),
                    provider_commitment: import_provider_commitment(session),
                    read_cap: TRANSCRIPT_READ_HARD_CAP_BYTES,
                    remaining_ms: 30_000,
                    transport: None,
                },
                descriptor,
                IMPORT_PREPARATION_HELPER_OUTPUT_CAP,
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap();
            let mut frame: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(frame["status"], "ok");
            assert_eq!(frame["raw_bytes"], source.len());
            assert!(frame.get("transport").is_none());
            assert!(frame.get("transport_required").is_none());
            // Mutate only this test-owned ordinary response, never a native
            // MAC or production helper. Exercise both advertised unavailability
            // and rejection of an invalid carrier version by the parent.
            if fault == 1 {
                frame["projection"]["codex_retention"] =
                    json!({"version":1,"turns":{},"unavailable":true});
            } else if fault == 2 {
                frame["projection"]["codex_retention"]["version"] = json!(255);
            }
            let frame: PreparationDescriptorHelperResponse =
                serde_json::from_slice(&serde_json::to_vec(&frame).unwrap()).unwrap();
            let PreparationDescriptorHelperResponse::Ok { projection, .. } = frame else {
                panic!("ordinary helper must prepare the source");
            };
            *projection
        }

        async fn claims(conn: &DatabaseConnection) -> Vec<String> {
            conn.query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT json_object('key',logical_turn_key,'version',coverage_schema_version,'digest',coverage_digest,'checkpoint',checkpoint_id,'revision',revision,'state',state,'owner',owner,'fence',fence_token) AS row FROM agent_coverage_claim ORDER BY logical_turn_key".to_owned(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get_by("row").unwrap())
            .collect()
        }

        #[tokio::test]
        #[ignore = "operator-local Codex acceptance; never run in CI"]
        async fn codex_summary_helper_unavailable_recovers_through_parent_and_store() {
            let program = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("libra");
            assert!(program.is_file());
            for fault in [1, 2] {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("repo");
                let home = temp.path().join("home");
                std::fs::create_dir_all(&root).unwrap();
                std::fs::create_dir_all(&home).unwrap();
                let root = root.canonicalize().unwrap();
                let init = run_cli(&program, &root, &home, &["init"]);
                assert!(
                    init.status.success(),
                    "{}",
                    String::from_utf8_lossy(&init.stderr)
                );
                let storage = root.join(util::ROOT_DIR);
                let conn = sea_orm::Database::connect(format!(
                    "sqlite://{}?mode=rw",
                    storage.join(util::DATABASE).display(),
                ))
                .await
                .unwrap();
                let scope = CaptureScope::resolve(&conn, &root).await.unwrap();
                let session = "123e4567-e89b-12d3-a456-426614174038";
                let mut rows = vec![
                    json!({"type":"session_meta","timestamp":"2026-10-19T01:00:00Z","payload":{"id":session,"cwd":root}}),
                ];
                for i in 0..2 {
                    rows.extend([
                        json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("fallback-{i}")}}),
                        json!({"type":"response_item","timestamp":"2026-10-19T01:00:01Z","payload":{"type":"message","role":"user","content":format!("question {i}")}}),
                        json!({"type":"response_item","timestamp":"2026-10-19T01:00:02Z","payload":{"type":"reasoning","id":format!("rs-fallback-{i}"),"summary":[{"type":"summary_text","text":format!("CX15-FALLBACK-{i}")}],"encrypted_content":format!("CX15-OPAQUE-{i}")}}),
                        json!({"type":"response_item","timestamp":"2026-10-19T01:00:03Z","payload":{"type":"message","role":"assistant","content":format!("answer {i}")}}),
                    ]);
                }
                rows.push(json!({"type":"session_end","timestamp":"2026-10-19T01:00:04Z","payload":{"type":"session_end","cwd":root}}));
                let source = (rows
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n")
                    .into_bytes();
                let mut original_claims = Vec::new();
                let mut original_checkpoints = std::collections::BTreeMap::new();
                for pass in 0..3 {
                    let mut projection = helper_projection(
                        &program,
                        &source,
                        session,
                        if pass == 0 { fault } else { 0 },
                    )
                    .await;
                    validate_prepared_runtime_response(AgentKind::Codex, &projection, false)
                        .unwrap();
                    let deadline =
                        CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
                    let retention =
                        consume_codex_retention(&mut projection, deadline.monotonic()).unwrap();
                    assert_eq!(retention.is_unavailable(), pass == 0);
                    assert_eq!(
                        projection.redaction_report["codex_reasoning"]["retention_unavailable"]
                            .as_bool()
                            .unwrap_or(false),
                        pass == 0
                    );
                    let result = crate::internal::ai::authorized_read::with_test_helper_program(
                        program.clone(),
                        async {
                            let source_preimage = import_source_preimage(
                                AgentKind::Codex,
                                "file",
                                "codex-retention-local-descriptor",
                                session,
                            )
                            .unwrap();
                            let source_commitment =
                                derive_capture_source_commitment_in_scope_until(
                                    &conn,
                                    &scope,
                                    &storage,
                                    &root,
                                    CaptureSourceCommitmentDomain::ImportSourceV2,
                                    &source_preimage,
                                    deadline.monotonic(),
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
                                derive_snapshot_content_commitment_in_scope_until(
                                    &conn,
                                    &scope,
                                    &storage,
                                    &root,
                                    &snapshot_preimage,
                                    deadline.monotonic(),
                                )
                                .await
                                .unwrap();
                            assert!(
                                projection
                                    .transcript_snapshot
                                    .as_mut()
                                    .unwrap()
                                    .bind_source_commitment(snapshot_commitment)
                            );
                            let mut request = import_request_from_projection(
                                AgentKind::Codex,
                                session.to_owned(),
                                "file".to_owned(),
                                source_commitment,
                                root.clone(),
                                projection,
                            );
                            request.capture_scope = Some(scope.clone());
                            let existing = load_existing_session_ownership_in_scope(
                                &conn,
                                AgentKind::Codex,
                                session,
                                &scope,
                            )
                            .await
                            .unwrap();
                            validate_scoped_prepared_existing_session(
                                &mut request,
                                existing.as_ref(),
                            )
                            .unwrap();
                            import_prepared_with_retention(
                                &conn,
                                &storage,
                                request,
                                deadline,
                                crate::internal::ai::subagent_content::SubagentDiscovery::default(),
                                None,
                                None,
                                Some(retention),
                            )
                            .await
                        },
                    )
                    .await
                    .unwrap();
                    assert_eq!(result.summary.partial, pass == 0);
                    assert_eq!(
                        result.summary.checkpoints_written,
                        if pass == 2 { 0 } else { 2 }
                    );
                    assert!(
                        ClientStorage::wait_for_background_tasks_until(
                            Instant::now() + Duration::from_secs(10)
                        )
                        .await
                    );
                    let current_claims = claims(&conn).await;
                    assert_eq!(current_claims.len(), 2);
                    if pass == 0 {
                        original_claims = current_claims;
                    } else {
                        assert_eq!(
                            current_claims, original_claims,
                            "supplements cannot rewrite semantic claims"
                        );
                    }
                    let checkpoints = conn.query_all_raw(Statement::from_string(conn.get_database_backend(), "SELECT checkpoint_id,metadata_blob_oid FROM agent_checkpoint ORDER BY rowid".to_owned())).await.unwrap();
                    assert_eq!(checkpoints.len(), if pass == 0 { 2 } else { 4 });
                    let mut thinking_exports = 0;
                    let mut all_exports = String::new();
                    for row in checkpoints {
                        let id: String = row.try_get_by("checkpoint_id").unwrap();
                        let exported = run_cli(
                            &program,
                            &root,
                            &home,
                            &["agent", "checkpoint", "export", &id],
                        );
                        assert!(exported.status.success());
                        let text = String::from_utf8(exported.stdout).unwrap();
                        thinking_exports += usize::from(text.contains("CX15-FALLBACK-"));
                        let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
                        if pass == 0 {
                            original_checkpoints.insert(id.clone(), (oid.clone(), text.clone()));
                        } else if let Some((original_oid, original_text)) =
                            original_checkpoints.get(&id)
                        {
                            assert_eq!(&oid, original_oid, "old metadata objects are immutable");
                            assert_eq!(&text, original_text, "old ordinary exports are immutable");
                        }
                        all_exports.push_str(&text);
                        let metadata: Value = serde_json::from_slice(
                            &crate::utils::object::read_git_object(
                                &storage,
                                &ObjectHash::from_str(&oid).unwrap(),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        assert!(!metadata.to_string().contains("CX15-FALLBACK-"));
                        if pass == 0 {
                            assert_eq!(
                                metadata["redaction_report"]["codex_reasoning"]["retention_unavailable"],
                                true
                            );
                        }
                    }
                    assert_eq!(thinking_exports, if pass == 0 { 0 } else { 2 });
                    for i in 0..2 {
                        assert_eq!(
                            all_exports.matches(&format!("CX15-FALLBACK-{i}")).count(),
                            usize::from(pass != 0)
                        );
                        assert_eq!(
                            all_exports.matches(&format!("CX15-OPAQUE-{i}")).count(),
                            usize::from(pass != 0)
                        );
                    }
                }
                conn.close().await.unwrap();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn cov2_runtime_response_required_and_native_projection_binding() {
        let (mut projection, binding, key, native_dto) =
            authenticated_reasoning_transport_test_fixture();
        assert!(projection.coverage_runtime.is_none());
        let old = serde_json::to_value(&projection).unwrap();
        assert!(
            old.get("coverage_runtime").is_none(),
            "None retains old native wire shape"
        );
        assert!(
            validate_prepared_runtime_response(AgentKind::ClaudeCode, &projection, true).is_ok()
        );
        assert!(
            validate_prepared_runtime_response(AgentKind::OpenCode, &projection, false).is_err()
        );
        projection.coverage_runtime = Some(
            serde_json::from_str(
                r#"{"version":1,"classifier_rejected":true,"source_partial":false,"turns":null}"#,
            )
            .unwrap(),
        );
        assert!(
            validate_prepared_runtime_response(AgentKind::OpenCode, &projection, false).is_ok()
        );
        assert!(
            validate_prepared_runtime_response(AgentKind::ClaudeCode, &projection, false).is_err()
        );
        let error = consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(native_dto),
        )
        .err()
        .unwrap();
        assert_transport_rejected_before_native_proof(&error);
        projection.coverage_runtime = Some(serde_json::from_str(
            r#"{"version":1,"classifier_rejected":false,"source_partial":false,"turns":[{"schema_version":2,"agent_kind":"opencode","logical_turn_key":"u","ordinal":0,"completeness":"complete","records":[{"type":"reasoning","provider":"opencode","source_kind":"reasoning","text":"visible"}]}]}"#).unwrap());
        assert!(
            validate_prepared_runtime_response(AgentKind::OpenCode, &projection, true).is_err(),
            "typed runtime cannot ride a native transport"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_descriptor_helper_carries_closed_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let storage = root.join(util::ROOT_DIR);
        std::fs::create_dir_all(storage.join("objects")).unwrap();
        let conn =
            crate::internal::db::create_database(storage.join(util::DATABASE).to_str().unwrap())
                .await
                .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "info":{"id":"pure-partial-store","directory":root},
            "messages":[{"id":"u","type":"user","text":"hi"},
              {"id":"a-u","type":"assistant","content":[
                {"id":"r-u","type":"reasoning","text":"before sk-ant-EXAMPLE0000000000000000 after"},
                {"id":"t-u","type":"text","text":"hello"}]},
              {"id":"i-u","type":"idle","outcome":"succeeded"}]
        })).unwrap();
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(&bytes).unwrap();
        source.seek(SeekFrom::Start(0)).unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("libra");
        assert!(executable.is_file());
        let mut command = tokio::process::Command::new(executable);
        command.arg(IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG);
        let response = run_import_preparation_descriptor_helper_bounded(
            command,
            PreparationDescriptorControl {
                agent_kind: "opencode".to_owned(),
                source_kind: "export".to_owned(),
                provider_commitment: import_provider_commitment("pure-partial-store"),
                read_cap: TRANSCRIPT_READ_HARD_CAP_BYTES,
                remaining_ms: 30_000,
                transport: None,
            },
            source,
            IMPORT_PREPARATION_HELPER_OUTPUT_CAP,
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(!String::from_utf8_lossy(&response).contains("sk-ant-EXAMPLE"));
        let frame: PreparationDescriptorHelperResponse = serde_json::from_slice(&response).unwrap();
        let PreparationDescriptorHelperResponse::Ok {
            projection,
            transport,
            transport_required,
            ..
        } = frame
        else {
            panic!("helper must prepare the source");
        };
        assert!(transport.is_none() && transport_required.is_none());
        validate_prepared_runtime_response(AgentKind::OpenCode, &projection, false).unwrap();
        assert!(import_runtime_has_typed_turns(
            projection.coverage_runtime.as_ref().unwrap()
        ));
    }

    #[cfg(unix)]
    fn reasoning_transport_test_projection() -> PreparedImportProjection {
        use crate::internal::ai::observed_agents::{Completeness, NormalizedTurn, SemanticRecord};

        let turn = NormalizedTurn {
            logical_turn_key: "synthetic-turn-0".to_owned(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: Some(1),
            ended_at: Some(2),
            records: vec![SemanticRecord::User {
                text: "ordinary synthetic import source".to_owned(),
            }],
        };
        let mut content = sha2::Sha256::new();
        content.update(turn.logical_turn_key.as_bytes());
        content.update([0]);
        content.update(turn.digest_hex().as_bytes());
        content.update([0xff]);
        PreparedImportProjection {
            content_digest: hex::encode(content.finalize()),
            started_at: 1,
            ended_at: 2,
            session_state: "stopped".to_owned(),
            stopped_at: Some(2),
            storage_commitment: [0x11; 32],
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0
            }),
            transcript_snapshot: Some(CaptureSnapshotProjection {
                completeness: CaptureSnapshotCompleteness::Complete,
                partial_reason: None,
                source: Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::TrustedExport,
                    identity: "not_retained:v1".to_owned(),
                    digest_sha256: Some(format!("sha256:{}", hex::encode([0x55; 32]))),
                    byte_len: Some(8),
                }),
                transcript_redacted_bytes: 8,
                redaction_match_count: 0,
                redaction_bytes_scanned: 8,
                redaction_bytes_redacted: 0,
            }),
            turn_boundaries: std::collections::BTreeMap::from([(
                turn.logical_turn_key.clone(),
                crate::internal::ai::agent_import::TurnBoundary {
                    started_at: 1,
                    ended_at: 2,
                },
            )]),
            turns: vec![turn],
            coverage_runtime: None,
            codex_retention: None,
        }
    }

    #[cfg(unix)]
    fn authenticated_reasoning_transport_test_fixture() -> (
        PreparedImportProjection,
        PreparationImportTransportBinding,
        ImportTransportKey,
        ImportTransportDto,
    ) {
        const SOURCE: &str = "og16-native-source";
        const CAPTURE: &str = "claude__og16-native-source";
        let key = ImportTransportKey::random().expect("fresh test transport key");
        let mut control = test_descriptor_control();
        control.transport = Some(PreparationImportTransportControl {
            version: 1,
            challenge: [0x44; 32],
            source_preimage_commitment: [0x22; 32],
            scope_digest: [0x33; 32],
            key: key.encode_for_helper(),
        });
        let binding = PreparationImportTransportBinding::from_control(&control)
            .expect("closed control")
            .expect("transport extension");
        let helper_key = control
            .transport
            .expect("helper key")
            .key
            .into_key()
            .expect("decode helper key");
        let projection = reasoning_transport_test_projection();
        let context = binding
            .context(AgentKind::ClaudeCode, SOURCE, &projection)
            .expect("bound context");
        let prepared = synthetic_native_preparation(
            Vec::new(),
            CAPTURE,
            SOURCE,
            vec![b"opaque-test-artifact".to_vec()],
        );
        let (_, disposition) = ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared,
            true,
            1024,
            "claude_code",
            CAPTURE,
            Some(SOURCE),
        )
        .expect("synthetic native fixture is source bound");
        let ReasoningSourceDisposition::NativeProven(native) = disposition else {
            panic!("synthetic fixture must own native reasoning");
        };
        let dto = seal_import_transport(helper_key, &context, native)
            .expect("production transport sealer accepts the test-only witness");
        let parent_projection = serde_json::from_slice(
            &serde_json::to_vec(&projection).expect("encode helper projection"),
        )
        .expect("decode helper projection in the parent");
        (parent_projection, binding, key, dto)
    }

    #[cfg(unix)]
    async fn authenticated_reasoning_transport_real_store_evidence()
    -> crate::internal::ai::agent_import::tests::NativeImportStoreEvidence {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let restored = consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(dto),
        )
        .expect("production parent authenticates the closed helper response")
        .expect("helper response contains an owned native batch");
        crate::internal::ai::agent_import::tests::import_verified_native_real_store_for_command_test(
            restored,
            b"opaque-test-artifact",
        )
        .await
        .expect("parent-restored witness reaches the real import reservation and shared store")
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_transport_production_consumer_accepts_authenticated_fixture() {
        let evidence = authenticated_reasoning_transport_real_store_evidence().await;
        assert_eq!(evidence.artifact_bytes, b"opaque-test-artifact");
        assert!(!evidence.artifact_oid.is_empty());
        assert_eq!(evidence.checkpoints_written, 1);
        assert_eq!(evidence.committed_claims, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_transport_identity_reaches_real_import_reservation() {
        let evidence = authenticated_reasoning_transport_real_store_evidence().await;
        assert_eq!(
            evidence.identity_kind,
            crate::internal::metadata::ReasoningIdentityKind::Nonempty
        );
        assert!(!evidence.identity_slot.is_empty());
        assert_eq!(
            evidence.identity_descriptor_mac,
            evidence.expected_descriptor_mac
        );
        assert_eq!(evidence.committed_claims, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn reasoning_transport_preserves_bound_native_proof() {
        let evidence = authenticated_reasoning_transport_real_store_evidence().await;
        assert_eq!(evidence.issuer_tag, "synthetic-test-only-v1");
        assert_eq!(evidence.logical_turn, "synthetic-turn-0");
        assert_eq!(
            evidence.artifact_locator,
            "claude_code:msg=0/part=0/metadata=signature"
        );
        let expected_wire_sha256: [u8; 32] = sha2::Sha256::digest(b"opaque-test-artifact").into();
        assert_eq!(evidence.original_wire_sha256, expected_wire_sha256);
        assert_ne!(evidence.source_binding_sha256, [0; 32]);
    }

    #[cfg(unix)]
    fn tamper_encoded_transport_context_sha256(
        dto: ImportTransportDto,
        index: usize,
    ) -> ImportTransportDto {
        let mut encoded = serde_json::to_value(dto).expect("encode production transport DTO");
        let context = encoded["context_sha256"]
            .as_array_mut()
            .expect("production encoder emits a context digest");
        let original = context[index]
            .as_u64()
            .expect("context digest byte is numeric");
        context[index] = serde_json::json!((original as u8) ^ 1);
        serde_json::from_value(encoded).expect("decode changed closed transport DTO")
    }

    #[cfg(unix)]
    fn assert_transport_rejected_before_native_proof(error: &anyhow::Error) {
        assert!(
            matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::AuthorizedReaderFailed)
            ),
            "the command must retain its stable authorized-reader error"
        );
        assert!(
            matches!(
                error.downcast_ref::<ReasoningTransportError>(),
                Some(ReasoningTransportError::Rejected)
            ),
            "context or request tamper must fail before native proof validation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_parent_rejects_request_mutation() {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let changed_dto = tamper_encoded_transport_context_sha256(dto, 0);
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(changed_dto),
        ) {
            Ok(_) => panic!("changed encoder-produced request context must be rejected"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&error);

        let (projection, mut binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        binding.request_digest[0] ^= 1;
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(dto),
        ) {
            Ok(_) => panic!("changed parent request digest must be rejected"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&error);
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_parent_rejects_source_mutation() {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let changed_dto = tamper_encoded_transport_context_sha256(dto, 1);
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(changed_dto),
        ) {
            Ok(_) => panic!("changed encoder-produced source context must be rejected"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&error);

        let (projection, mut binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        binding.source_preimage_commitment[0] ^= 1;
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(dto),
        ) {
            Ok(_) => panic!("changed parent source commitment must be rejected"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&error);
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_parent_rejects_payload_mutation() {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let mut value = serde_json::to_value(dto).expect("serialize closed transport DTO");
        let encoded = value["items"][0]["bytes_base64"]
            .as_str()
            .expect("one nonempty encoded artifact");
        value["items"][0]["bytes_base64"] = serde_json::Value::String(format!("A{encoded}"));
        let altered = serde_json::from_value(value).expect("still a closed DTO shape");
        assert!(
            consume_prepared_reasoning_transport(
                key,
                &binding,
                AgentKind::ClaudeCode,
                "og16-native-source",
                &projection,
                Some(true),
                Some(altered),
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_nonempty_missing_proof_reaches_parent_rejection() {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let helper_key = key
            .encode_for_helper()
            .into_key()
            .expect("one test-only helper key copy");
        let context = binding
            .context(AgentKind::ClaudeCode, "og16-native-source", &projection)
            .expect("bound parent context");
        let invalid = invalidate_import_transport_native_proof_for_test(dto, helper_key, &context)
            .expect("valid-MAC test fixture with one invalid native proof");
        let encoded = serde_json::to_value(&invalid).expect("encode transport fixture");
        assert_eq!(
            encoded["items"].as_array().map(Vec::len),
            Some(1),
            "the proof rejection must not be vacuous on an empty collection"
        );
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(invalid),
        ) {
            Ok(_) => panic!("authenticated but unproven native item must be rejected"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::AuthorizedReaderFailed)
            ),
            "the command must retain its stable authorized-reader error"
        );
        assert!(
            matches!(
                error.downcast_ref::<ReasoningTransportError>(),
                Some(ReasoningTransportError::NativeProofRejected)
            ),
            "authenticated nonempty transport must reach native proof rejection"
        );

        let (projection, mut binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        binding.request_digest[0] ^= 1;
        let request_error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(dto),
        ) {
            Ok(_) => panic!("changed request must fail before native proof validation"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&request_error);

        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let changed_context = tamper_encoded_transport_context_sha256(dto, 2);
        let context_error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(changed_context),
        ) {
            Ok(_) => panic!("changed DTO context must fail before native proof validation"),
            Err(error) => error,
        };
        assert_transport_rejected_before_native_proof(&context_error);
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_parent_rejects_missing_required_transport() {
        let (projection, binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let missing = consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            None,
        );
        assert!(
            missing.is_err(),
            "a native-marked response cannot omit its DTO"
        );
        let key = ImportTransportKey::random().expect("fresh parent key");
        let unsolicited = consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            None,
            Some(dto),
        );
        assert!(
            unsolicited.is_err(),
            "an unmarked response cannot inject a native DTO"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_control_extension_has_closed_version() {
        let key = ImportTransportKey::random().expect("fresh key");
        let mut control = test_descriptor_control();
        control.transport = Some(PreparationImportTransportControl {
            version: 1,
            challenge: [0x44; 32],
            source_preimage_commitment: [0x22; 32],
            scope_digest: [0x33; 32],
            key: key.encode_for_helper(),
        });
        assert!(
            PreparationImportTransportBinding::from_control(&control)
                .expect("v1 is supported")
                .is_some()
        );
        let mut value = serde_json::to_value(&control).expect("encode closed control");
        value["transport"]["version"] = serde_json::json!(2);
        let old = serde_json::from_value::<PreparationDescriptorControl>(value.clone())
            .expect("closed shape with an unsupported version");
        assert!(PreparationImportTransportBinding::from_control(&old).is_err());
        value["transport"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<PreparationDescriptorControl>(value).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reasoning_transport_wire_key_is_redacted_from_debug_errors_and_stdout() {
        let (projection, mut binding, key, dto) = authenticated_reasoning_transport_test_fixture();
        let encoded_key = key.encode_for_helper();
        let key_canary = serde_json::to_string(&encoded_key).expect("encode wire key");
        let debug_key = format!("{encoded_key:?}");
        assert!(!debug_key.contains(&key_canary));
        let challenge_canary = serde_json::to_string(&[0x44_u8; 32]).expect("encode challenge");
        let response = PreparationDescriptorHelperResponse::Ok {
            projection: Box::new(projection),
            raw_bytes: 8,
            transport_required: Some(true),
            transport: Some(dto),
        };
        let stdout = serde_json::to_string(&response).expect("encode helper response");
        assert!(!stdout.contains(&key_canary));
        assert!(!stdout.contains(&challenge_canary));
        let PreparationDescriptorHelperResponse::Ok {
            projection,
            transport: Some(dto),
            ..
        } = response
        else {
            panic!("fixture response must carry its sealed transport");
        };
        binding.request_digest[0] ^= 1;
        let error = match consume_prepared_reasoning_transport(
            key,
            &binding,
            AgentKind::ClaudeCode,
            "og16-native-source",
            &projection,
            Some(true),
            Some(dto),
        ) {
            Ok(_) => panic!("mutated request must fail closed"),
            Err(error) => error,
        };
        let diagnostic = format!("{error:#}");
        assert!(!diagnostic.contains(&key_canary));
        assert!(!diagnostic.contains(&challenge_canary));
    }

    #[test]
    fn source_commitment_failure_preserves_deadline_classification() {
        let future = Instant::now() + Duration::from_secs(10);
        let typed_deadline =
            anyhow::Error::new(CaptureFinalCommitAuthorizationError::DeadlineElapsed);
        assert!(matches!(
            import_commitment_failure(typed_deadline, future),
            ImportError::DeadlineExceeded
        ));

        let expired = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            import_commitment_failure(anyhow::anyhow!("commitment failed"), expired),
            ImportError::DeadlineExceeded
        ));

        assert!(matches!(
            import_commitment_failure(anyhow::anyhow!("commitment failed"), future),
            ImportError::AuthorizedReaderFailed
        ));
    }

    #[test]
    fn import_fault_controls_are_in_process_test_state() {
        let (source_reached_sender, source_reached_receiver) = std::sync::mpsc::channel();
        let (source_resume_sender, source_resume_receiver) = std::sync::mpsc::channel();
        let (barrier_reached_sender, barrier_reached_receiver) = std::sync::mpsc::channel();
        let (barrier_resume_sender, barrier_resume_receiver) = std::sync::mpsc::channel();
        let _reset = test_support::install(test_support::ImportTestControls {
            total_deadline: Some(Duration::from_millis(25)),
            batch_raw_byte_cap: Some(123),
            fail_index_tombstone_lookup: true,
            preparation_response_read_delay: Some(Duration::from_millis(1)),
            source_open_pause: Some(test_support::TestPause {
                reached: source_reached_sender,
                resume: source_resume_receiver,
            }),
            index_barrier_pause: Some(test_support::TestPause {
                reached: barrier_reached_sender,
                resume: barrier_resume_receiver,
            }),
            ..test_support::ImportTestControls::default()
        });

        assert_eq!(import_total_deadline(), Duration::from_millis(25));
        assert_eq!(batch_raw_byte_cap(), 123);
        assert!(test_support::fail_index_tombstone_lookup());
        assert_eq!(
            test_support::preparation_response_read_delay(),
            Some(Duration::from_millis(1))
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        let source_pause =
            std::thread::spawn(move || import_test_pause_after_source_open(deadline));
        source_reached_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("source-open in-process pause is reached");
        source_resume_sender
            .send(())
            .expect("source-open pause is still waiting");
        source_pause
            .join()
            .expect("source-open test pause worker does not panic")
            .expect("source-open test pause resumes");

        let deadline = Instant::now() + Duration::from_secs(1);
        let barrier_pause =
            std::thread::spawn(move || import_test_pause_after_index_barrier(deadline));
        barrier_reached_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("index-barrier in-process pause is reached");
        barrier_resume_sender
            .send(())
            .expect("index-barrier pause is still waiting");
        barrier_pause
            .join()
            .expect("index-barrier test pause worker does not panic")
            .expect("index-barrier test pause resumes");
    }

    fn import_summary(parent: usize, subagent: usize) -> DetailedImportSummary {
        DetailedImportSummary {
            summary: ImportSummary {
                session_id: "session".to_string(),
                agent_kind: "claude_code".to_string(),
                turns_seen: 1,
                checkpoints_written: parent,
                skipped_covered: 0,
                skipped_inflight: 0,
                conflicted: 0,
                partial: false,
            },
            subagent_checkpoints_written: subagent,
            import_identity_id: "import-test".to_string(),
            import_fence_token: 1,
        }
    }

    #[test]
    fn child_only_import_is_not_reported_as_noop() {
        assert_eq!(
            BatchResult::complete(import_summary(0, 1)).status,
            "imported"
        );
        assert_eq!(BatchResult::complete(import_summary(0, 0)).status, "noop");
    }

    #[cfg_attr(windows, allow(dead_code))]
    struct ImportIndexRepairCheckpointFixture {
        checkpoint_id: String,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
    }

    /// Write the smallest E4 checkpoint layout that import replay recognizes:
    /// a manifest-bearing checkpoint tree plus a valid traces commit.
    #[cfg_attr(windows, allow(dead_code))]
    fn write_import_index_repair_checkpoint(
        repo_path: &Path,
        checkpoint_id: &str,
    ) -> ImportIndexRepairCheckpointFixture {
        let write_tree = |entries: &[(&str, &str, &str)]| {
            let mut body = Vec::new();
            for (mode, name, oid) in entries {
                body.extend_from_slice(mode.as_bytes());
                body.push(b' ');
                body.extend_from_slice(name.as_bytes());
                body.push(0);
                body.extend_from_slice(&hex::decode(oid).expect("fixture tree OID is hex"));
            }
            crate::utils::object::write_git_object(repo_path, "tree", &body)
                .expect("write import repair fixture tree")
                .to_string()
        };

        let metadata = serde_json::json!({
            "schema_version": 2,
            "checkpoint_id": checkpoint_id,
            "session_id": "claude_code__import_repair_atomic",
            "agent_kind": "claude_code",
        })
        .to_string();
        let metadata_blob_oid =
            crate::utils::object::write_git_object(repo_path, "blob", metadata.as_bytes())
                .expect("write import repair fixture metadata")
                .to_string();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "entries": {
                "metadata": {
                    "path": "metadata.json",
                    "oid": metadata_blob_oid,
                    "byte_len": metadata.len(),
                }
            }
        })
        .to_string();
        let manifest_oid =
            crate::utils::object::write_git_object(repo_path, "blob", manifest.as_bytes())
                .expect("write import repair fixture manifest")
                .to_string();
        let inner_tree = write_tree(&[
            ("100644", "manifest.json", &manifest_oid),
            ("100644", "metadata.json", &metadata_blob_oid),
        ]);
        let prefix_tree = write_tree(&[("40000", &checkpoint_id[2..], &inner_tree)]);
        let checkpoint_tree = write_tree(&[("40000", &checkpoint_id[..2], &prefix_tree)]);
        let tree_oid = write_tree(&[("40000", "checkpoint", &checkpoint_tree)]);
        let commit = format!(
            "tree {tree_oid}\nauthor Libra <traces@libra> 0 +0000\ncommitter Libra <traces@libra> 0 +0000\n\nimport repair fixture\n"
        );
        let traces_commit =
            crate::utils::object::write_git_object(repo_path, "commit", commit.as_bytes())
                .expect("write import repair fixture traces commit")
                .to_string();

        ImportIndexRepairCheckpointFixture {
            checkpoint_id: checkpoint_id.to_string(),
            tree_oid,
            metadata_blob_oid,
            traces_commit,
        }
    }

    #[test]
    #[serial_test::serial(cwd, env)]
    fn index_drain_timeout_remains_a_structured_batch_failure() {
        let candidate = Candidate {
            kind: AgentKind::ClaudeCode,
            provider_session_id: "provider-session".to_string(),
            path: None,
        };
        let detailed = import_summary(1, 1);
        let mut results = vec![BatchResult::complete(import_summary(1, 0))];
        let mut partial_results = Vec::new();
        let mut skipped = Vec::new();
        let mut failures = Vec::new();
        record_candidate_result(
            &candidate,
            false,
            true,
            Ok(detailed),
            &mut results,
            &mut partial_results,
            &mut skipped,
            &mut failures,
        );
        assert_eq!(results.len(), 1, "earlier batch results must be preserved");
        assert_eq!(
            partial_results.len(),
            1,
            "durable progress must be retained"
        );
        assert!(partial_results[0].summary.partial);
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].error_code,
            StableErrorCode::AgentImportPartialBatch
        );
        assert_eq!(failures[0].status, "failed");
        assert_eq!(
            failures[0].session_id, "sha256:ed3704917254",
            "public import failures keep the documented short hashed session id"
        );
    }

    #[test]
    fn public_batch_items_report_short_hashed_session_id() {
        let candidate = |provider_session_id: &str| Candidate {
            kind: AgentKind::ClaudeCode,
            provider_session_id: provider_session_id.to_string(),
            path: Some(PathBuf::from("/private/provider/root/session.jsonl")),
        };
        let error: anyhow::Error = ImportError::RepositoryConflict.into();
        let failure = safe_failure(&candidate("provider-session"), &error);
        let skip = safe_skip(&candidate("provider-session"), &error);
        // SHA-256("provider-session")[..6], independently computed.
        assert_eq!(failure.session_id, "sha256:ed3704917254");
        assert_eq!(skip.session_id, failure.session_id);
        assert_eq!(skip.status, "skipped");
        assert_eq!(
            skip.reason_code,
            StableErrorCode::AgentImportRepositoryConflict
        );
        for public in [&failure.session_id, &skip.session_id] {
            let digest = public
                .strip_prefix("sha256:")
                .expect("hashed session id prefix");
            assert_eq!(digest.len(), 12);
            assert!(
                digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
            assert!(!public.contains("provider-session") && !public.contains("/private"));
        }
        // A per-item id, not a constant sentinel.
        assert_eq!(
            safe_failure(&candidate("other-session"), &error).session_id,
            "sha256:5f711b23ee4f"
        );
    }

    #[test]
    fn parent_and_child_share_one_per_candidate_read_allowance() {
        assert_eq!(remaining_candidate_read_allowance(100, 40), 60);
        assert_eq!(remaining_candidate_read_allowance(100, 100), 0);
        assert_eq!(remaining_candidate_read_allowance(100, 140), 0);
    }

    #[test]
    fn opencode_session_ids_use_the_exporter_grammar() {
        assert!(session_id_is_valid_for_kind(
            "safe-id_1",
            AgentKind::OpenCode
        ));
        assert!(!session_id_is_valid_for_kind(
            "legacy.identifier",
            AgentKind::OpenCode
        ));
        assert!(!session_id_is_valid_for_kind(
            &"a".repeat(65),
            AgentKind::OpenCode
        ));
        assert!(session_id_is_valid_for_kind(
            "legacy.identifier",
            AgentKind::ClaudeCode
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn failed_subagent_discovery_keeps_full_reserved_allowance_charged() {
        let mut cumulative = 10_u64;
        reserve_subagent_input_allowance(&mut cumulative, 20).expect("reserve allowance");
        // A discovery error returns before settlement, so the conservative
        // reservation remains charged to the batch.
        assert_eq!(cumulative, 30);

        settle_subagent_input_allowance(&mut cumulative, 20, 7)
            .expect("settle successful discovery");
        assert_eq!(cumulative, 17);
        assert!(settle_subagent_input_allowance(&mut cumulative, 5, 6).is_err());
    }

    #[test]
    fn registered_import_helper_is_required_before_any_spawn() {
        let error = require_registered_import_helper_command(None)
            .expect_err("an embedded host has no registered Libra helper");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::AuthorizedReaderUnavailable)
        ));
    }

    #[test]
    fn helper_error_frames_are_content_free_and_reject_extra_payload() {
        let discovery = serde_json::to_vec(&DiscoveryHelperResponse::Error {
            reason: DiscoveryRejection::SessionNotFound,
        })
        .expect("serialize bounded discovery error");
        assert_eq!(
            discovery,
            br#"{"status":"error","reason":"session_not_found"}"#
        );
        for frame in [
            br#"{"status":"error","reason":"session_not_found","message":"private-path"}"#
                .as_slice(),
            br#"{"status":"error","reason":"private-path"}"#.as_slice(),
            br#"{"status":"error","stable_code":"LBR-AGENT-020"}"#.as_slice(),
        ] {
            assert!(serde_json::from_slice::<DiscoveryHelperResponse>(frame).is_err());
        }

        let repair = serde_json::to_vec(&IndexRepairHelperResponse::Error {})
            .expect("serialize bounded repair error");
        assert_eq!(repair, br#"{"status":"error"}"#);
        assert!(
            serde_json::from_slice::<IndexRepairHelperResponse>(
                br#"{"status":"error","message":"private-path"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn preparation_error_wire_contract_preserves_safe_import_error_kinds() {
        for (error, expected) in [
            (
                ImportError::AuthorizedReaderUnavailable,
                PreparationImportErrorKind::SourceAuthorization,
            ),
            (
                ImportError::AuthorizedReaderFailed,
                PreparationImportErrorKind::SourceAuthorization,
            ),
            (
                ImportError::FutureTimestamp,
                PreparationImportErrorKind::FutureTimestamp,
            ),
        ] {
            let error: anyhow::Error = error.into();
            assert_eq!(preparation_error_kind(&error), Some(expected));
            assert!(matches!(
                preparation_error(expected),
                ImportError::SourceAuthorization | ImportError::FutureTimestamp
            ));
        }
        let future: anyhow::Error = ImportError::FutureTimestamp.into();
        assert_eq!(
            stable_code_for_error(&future),
            StableErrorCode::AgentImportPartialBatch
        );
    }

    #[cfg(unix)]
    fn write_import_helper_script(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create import helper script directory");
        let script = dir.path().join("import-helper.sh");
        // The production spawn clears the environment and switches to `/`,
        // so the script carries its own search path and test marker rather
        // than inheriting them from the test process.
        let marker = script.with_extension("pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nPATH=/usr/bin:/bin\nLIBRA_IMPORT_HELPER_TEST_MARKER='{}'\n{body}\n",
                marker.display()
            ),
        )
        .expect("write import helper script");
        let mut permissions = std::fs::metadata(&script)
            .expect("read import helper script permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions)
            .expect("make import helper script executable");
        (dir, script)
    }

    #[cfg(unix)]
    fn import_helper_script_command(script: &Path, marker: &Path) -> tokio::process::Command {
        // Inherited variables never reach the helper (`env_clear`), so the
        // marker is embedded by `write_import_helper_script` instead.
        assert_eq!(marker, script.with_extension("pid"));
        tokio::process::Command::new(script)
    }

    #[cfg(unix)]
    fn test_descriptor_control() -> PreparationDescriptorControl {
        PreparationDescriptorControl {
            agent_kind: "claude-code".to_string(),
            source_kind: "file".to_string(),
            provider_commitment: [7; 32],
            read_cap: 1024,
            remaining_ms: 1_000,
            transport: None,
        }
    }

    #[cfg(unix)]
    fn empty_test_descriptor() -> std::fs::File {
        tempfile::tempfile().expect("create anonymous descriptor helper input")
    }

    // These cases intentionally exercise a subprocess and process-group reaping.
    // Leave scheduling headroom when the full unit suite is running in parallel.
    #[cfg(unix)]
    const IMPORT_HELPER_TEST_DEADLINE: Duration = Duration::from_secs(5);

    #[cfg(unix)]
    async fn wait_for_import_helper_pid(marker: &Path) -> u32 {
        let deadline = Instant::now() + IMPORT_HELPER_TEST_DEADLINE;
        loop {
            if let Ok(value) = std::fs::read_to_string(marker)
                && let Ok(pid) = value.trim().parse::<u32>()
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "import helper did not publish its test PID"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    fn import_helper_pid_is_live(pid: u32) -> bool {
        // SAFETY: signal 0 only probes the explicitly test-created process;
        // it neither changes process state nor targets a process group.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        error.raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(unix)]
    async fn wait_for_import_helper_exit(pid: u32) {
        let deadline = Instant::now() + IMPORT_HELPER_TEST_DEADLINE;
        while import_helper_pid_is_live(pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !import_helper_pid_is_live(pid),
            "import helper process {pid} survived cancellation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preparation_helper_rejects_oversized_stdout_with_a_fixed_error() {
        let (_dir, script) = write_import_helper_script("printf '0123456789abcdef'");
        let marker = script.with_extension("pid");
        let error = run_import_preparation_descriptor_helper_bounded(
            import_helper_script_command(&script, &marker),
            test_descriptor_control(),
            empty_test_descriptor(),
            8,
            Instant::now() + IMPORT_HELPER_TEST_DEADLINE,
        )
        .await
        .expect_err("oversized helper stdout must be rejected");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::AuthorizedReaderFailed)
        ));
    }

    /// PI-10: the whole `agent import --agent pi --path` route. The CLI only
    /// opens the Pi file; the preparation helper reads it. With an injected
    /// preparation helper that stalls while reading that file (other helper
    /// calls run the real binary), the command returns at its import
    /// deadline, the stalled helper is reaped, and the file was opened once.
    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn pi_explicit_import_stalled_preparation_read_stops_the_command_at_its_deadline() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use crate::internal::{
            ai::authorized_read,
            worktree_scope::{RequestScope, with_request_scope},
        };

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        crate::utils::test::setup_with_new_libra_in(&root).await;
        let real = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("libra");
        assert!(real.is_file(), "the Libra binary must be built first");
        let id = "019a3c2e-7b10-7000-8000-0000000000b1";
        let path = root.join(format!("2026-10-31T00-00-00-000Z_{id}.jsonl"));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"type": "session", "version": 3, "id": id,
                    "timestamp": "2026-10-31T00:00:00.000Z", "cwd": root.to_str().unwrap()})
            ),
        )
        .unwrap();
        let (_dir, script) = write_import_helper_script(&format!(
            "if [ \"$1\" = '{IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG}' ]; then\n  printf '%s' \"$$\" > \"$LIBRA_IMPORT_HELPER_TEST_MARKER\"\n  head -c 1 > /dev/null\n  sleep 60\n  exit 1\nfi\nexec '{}' \"$@\"",
            real.display()
        ));
        let marker = script.with_extension("pid");
        let _reset = test_support::install(test_support::ImportTestControls {
            total_deadline: Some(Duration::from_secs(8)),
            ..test_support::ImportTestControls::default()
        });
        let reads = Arc::new(AtomicUsize::new(0));
        let args = selector_args(|args| {
            args.all = false;
            args.agent = Some("pi".to_string());
            args.path = Some(path.clone());
            args.yes = true;
        });
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };
        let started = Instant::now();
        let result = with_request_scope(
            Some(RequestScope::resolve(root.clone()).unwrap()),
            PI_SOURCE_READS.scope(
                reads.clone(),
                authorized_read::with_test_helper_program(
                    script.clone(),
                    execute_safe(args, &output),
                ),
            ),
        )
        .await;
        let elapsed = started.elapsed();
        assert!(result.is_err(), "a stalled Pi read must not succeed");
        assert!(
            elapsed >= Duration::from_secs(7),
            "the helper stalled until the deadline, not an earlier failure: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "the command outlived its deadline: {elapsed:?}"
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "the file is opened once, in the CLI"
        );
        let pid = wait_for_import_helper_pid(&marker).await;
        wait_for_import_helper_exit(pid).await;
    }

    /// PI-10: the CLI process cannot read a Pi session's bytes. The file is
    /// opened inside the private `pi_held_source` module, so no raw handle
    /// exists outside it; the `PiHeldSource` it returns implements neither
    /// `Read` nor `Seek` (checked at compile time below: either impl would make
    /// the probe call ambiguous); the module defines only `open` and
    /// `into_helper_stdin` and takes no byte (no read, seek, or buffering call
    /// in it); and production code converts the source only once, into the
    /// helper's standard input (`From<PreparationInput> for Stdio`, used only
    /// by the spawn's `.stdin(Stdio::from(source))`), so the CLI never holds
    /// it as a `File`. A read in the CLI would need a new impl, a new accessor,
    /// a read inside the module, or a second conversion, and this test fails
    /// on each.
    #[test]
    fn pi_held_source_cannot_be_read_by_the_cli() {
        trait NotReadable<Marker> {
            fn probe() {}
        }
        impl<T: ?Sized> NotReadable<()> for T {}
        struct ViaRead;
        impl<T: ?Sized + std::io::Read> NotReadable<ViaRead> for T {}
        struct ViaSeek;
        impl<T: ?Sized + std::io::Seek> NotReadable<ViaSeek> for T {}
        <PiHeldSource as NotReadable<_>>::probe();

        let source = include_str!("import.rs");
        let production = source
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or_default();
        assert_eq!(
            production.matches(".into_helper_stdin()").count(),
            1,
            "the held Pi source is converted only once"
        );
        let conversion = production
            .find("impl From<PreparationInput> for Stdio {")
            .expect("the helper-input conversion");
        assert!(
            production[conversion..].lines().take(8).any(
                |line| line.trim() == "PreparationInput::Pi(held) => held.into_helper_stdin(),"
            ),
            "the one conversion makes the helper's standard input"
        );
        assert_eq!(
            production.matches("Stdio::from(source)").count(),
            1,
            "a `PreparationInput` becomes a `Stdio` only in the helper spawn"
        );
        let spawn = production
            .find(".stdin(Stdio::from(source))")
            .expect("the helper spawn sets its standard input");
        let helper = production
            .find("async fn run_import_preparation_descriptor_helper_bounded(")
            .expect("the preparation helper spawn function");
        assert!(
            helper < spawn && !production[helper..spawn].contains("\nasync fn "),
            "the conversion is in the preparation helper spawn"
        );
        for raw_exit in [
            "into_helper_input",
            "-> std::fs::File {\n            self.0",
        ] {
            assert!(
                !production.contains(raw_exit),
                "the held source must not yield a `File`"
            );
        }
        let module_start = production
            .find("mod pi_held_source {")
            .expect("the held-source module");
        let module_end = production[module_start..]
            .find("\nuse pi_held_source::PiHeldSource;")
            .map(|end| module_start + end)
            .expect("the held-source module end");
        let module: String = production[module_start..module_end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            module.matches("fn ").count(),
            2,
            "the module defines only `open` and `into_helper_stdin`"
        );
        assert!(module.contains("fn open(") && module.contains("fn into_helper_stdin("));
        let words = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").expect("word pattern");
        for word in words.find_iter(&module).map(|found| found.as_str()) {
            assert!(
                ![
                    "Read",
                    "Seek",
                    "BufRead",
                    "BufReader",
                    "read",
                    "read_to_end",
                    "read_to_string",
                    "read_exact",
                    "read_at",
                    "read_vectored",
                    "seek",
                    "rewind",
                    "stream_position",
                    "take",
                    "bytes",
                    "copy",
                    "mmap",
                    "try_clone",
                    "as_raw_fd",
                    "as_fd",
                ]
                .contains(&word),
                "the held-source module must take no byte, but uses `{word}`"
            );
        }
    }

    /// PI-10: a Pi session file is read only by the killable preparation
    /// helper, never by the CLI process. A helper that stalls while reading
    /// the opened Pi file is killed at the import deadline: the import stops
    /// with `DeadlineExceeded` and the helper process is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn pi_session_read_stalled_in_the_helper_stops_at_the_deadline() {
        let sessions = tempfile::tempdir().unwrap();
        let id = "019a3c2e-7b10-7000-8000-0000000000b1";
        let path = sessions
            .path()
            .canonicalize()
            .unwrap()
            .join(format!("2026-10-31T00-00-00-000Z_{id}.jsonl"));
        std::fs::write(&path, b"{\"type\":\"session\"}\n").unwrap();
        let candidate = Candidate {
            kind: AgentKind::Pi,
            provider_session_id: id.to_string(),
            path: Some(path),
        };
        let (held, _) = open_pi_session_file(&candidate).unwrap();
        let (_dir, script) = write_import_helper_script(
            "printf '%s' \"$$\" > \"$LIBRA_IMPORT_HELPER_TEST_MARKER\"\nhead -c 1 > /dev/null\nsleep 30",
        );
        let marker = script.with_extension("pid");
        let mut control = test_descriptor_control();
        control.agent_kind = "pi".to_string();
        let started = Instant::now();
        let error = run_import_preparation_descriptor_helper_bounded(
            import_helper_script_command(&script, &marker),
            control,
            held,
            64,
            started + Duration::from_millis(500),
        )
        .await
        .expect_err("a stalled Pi read must stop at the deadline");
        assert!(
            matches!(
                error.downcast_ref::<ImportError>(),
                Some(ImportError::DeadlineExceeded)
            ),
            "{error:#}"
        );
        assert!(started.elapsed() < IMPORT_HELPER_TEST_DEADLINE);
        let pid = wait_for_import_helper_pid(&marker).await;
        wait_for_import_helper_exit(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn outer_cancellation_reaps_the_preparation_process_group() {
        let (_dir, script) = write_import_helper_script(
            "printf '%s' \"$$\" > \"$LIBRA_IMPORT_HELPER_TEST_MARKER\"\nsleep 30",
        );
        let marker = script.with_extension("pid");
        let task = tokio::spawn(run_import_preparation_descriptor_helper_bounded(
            import_helper_script_command(&script, &marker),
            test_descriptor_control(),
            empty_test_descriptor(),
            64,
            Instant::now() + IMPORT_HELPER_TEST_DEADLINE,
        ));
        let pid = wait_for_import_helper_pid(&marker).await;
        task.abort();
        assert!(
            task.await
                .expect_err("cancelled helper task must not complete")
                .is_cancelled()
        );
        wait_for_import_helper_exit(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preparation_helper_kills_descendant_that_holds_stdout_after_leader_exit() {
        let (_dir, script) = write_import_helper_script(
            "sleep 30 &\nprintf '%s' \"$!\" > \"$LIBRA_IMPORT_HELPER_TEST_MARKER\"\nexit 0",
        );
        let marker = script.with_extension("pid");
        let deadline = Instant::now() + IMPORT_HELPER_TEST_DEADLINE;
        let task = tokio::spawn(run_import_preparation_descriptor_helper_bounded(
            import_helper_script_command(&script, &marker),
            test_descriptor_control(),
            empty_test_descriptor(),
            64,
            deadline,
        ));
        let descendant_pid = wait_for_import_helper_pid(&marker).await;
        let error = task
            .await
            .expect("helper runner task joins")
            .expect_err("inherited stdout must not outlive the helper deadline");
        assert!(matches!(
            error.downcast_ref::<ImportError>(),
            Some(ImportError::DeadlineExceeded)
        ));
        wait_for_import_helper_exit(descendant_pid).await;
    }

    #[cfg_attr(windows, allow(dead_code))]
    struct TestHomeGuard(Option<std::ffi::OsString>);

    impl TestHomeGuard {
        #[cfg_attr(windows, allow(dead_code))]
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: serial test below restores the process environment.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self(previous)
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            // SAFETY: serial test below restores the process environment.
            unsafe {
                match &self.0 {
                    Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    #[test]
    fn agent_import_requires_explicit_selector() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: ImportArgs,
        }
        assert!(Wrapper::try_parse_from(["test"]).is_err());
        assert!(Wrapper::try_parse_from(["test", "--all", "--session", "x"]).is_err());
    }

    #[test]
    #[serial_test::serial(cwd)]
    fn agent_import_path_needs_agent() {
        let args = ImportArgs {
            session: None,
            path: Some(PathBuf::from("x.jsonl")),
            since: None,
            all: false,
            agent: None,
            limit: DEFAULT_IMPORT_LIMIT,
            cursor: None,
            yes: true,
            restore_erased: false,
            codex_index_backup: None,
        };
        let error = validate_discovery_selector(&args).expect_err("--path needs --agent");
        assert_eq!(error.message(), "--path requires --agent");
        assert_eq!(error.stable_code(), StableErrorCode::CliInvalidArguments);
        assert_eq!(error.kind(), CliErrorKind::CommandUsage);
        assert!(matches!(
            discover(
                &args,
                Path::new("."),
                Instant::now() + Duration::from_secs(1)
            ),
            Err(DiscoveryRejection::SelectorRejected)
        ));
    }

    /// PI-10 AC1: without consent, `agent import --agent pi --path` never
    /// enters the Pi session reader, so its read count stays zero and the
    /// refusal is the consent requirement. With `--yes` the same file is read
    /// exactly once, which shows the count observes the reader.
    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn pi_explicit_import_refused_consent_counts_zero_source_reads() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use crate::internal::{
            ai::authorized_read,
            worktree_scope::{RequestScope, with_request_scope},
        };

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        crate::utils::test::setup_with_new_libra_in(&root).await;
        let program = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("libra");
        assert!(program.is_file(), "the Libra binary must be built first");
        let path = root.join("2026-10-31T00-00-00-000Z_019a3c2e-7b10-7000-8000-0000000000b1.jsonl");
        std::fs::write(&path, b"{\"type\":\"session\"}\n").unwrap();
        let run = |yes: bool| {
            let reads = Arc::new(AtomicUsize::new(0));
            let args = selector_args(|args| {
                args.all = false;
                args.agent = Some("pi".to_string());
                args.path = Some(path.clone());
                args.yes = yes;
            });
            let root = root.clone();
            let program = program.clone();
            let counted = reads.clone();
            async move {
                let output = OutputConfig {
                    quiet: true,
                    ..OutputConfig::default()
                };
                let result = with_request_scope(
                    Some(RequestScope::resolve(root).unwrap()),
                    PI_SOURCE_READS.scope(
                        counted,
                        authorized_read::with_test_helper_program(
                            program,
                            execute_safe(args, &output),
                        ),
                    ),
                )
                .await;
                (result, reads.load(Ordering::SeqCst))
            }
        };
        let (refused, reads) = run(false).await;
        let error = refused.expect_err("consent is required");
        assert!(error.to_string().contains("--yes"), "{error}");
        assert_eq!(reads, 0, "the source was read before consent");
        let (_, reads) = run(true).await;
        assert_eq!(reads, 1, "with consent the file is read once");
    }

    fn selector_args(configure: impl FnOnce(&mut ImportArgs)) -> ImportArgs {
        let mut args = ImportArgs {
            session: None,
            path: None,
            since: None,
            all: true,
            agent: None,
            limit: DEFAULT_IMPORT_LIMIT,
            cursor: None,
            yes: true,
            restore_erased: false,
            codex_index_backup: None,
        };
        configure(&mut args);
        args
    }

    #[test]
    fn argv_selector_errors_keep_fixed_actionable_messages() {
        let cases: Vec<(ImportArgs, &str)> = vec![
            (
                selector_args(|args| {
                    args.limit = 0;
                    args.agent = Some("gemini".to_string());
                }),
                "--limit must be between 1 and 100",
            ),
            (
                selector_args(|args| args.limit = MAX_IMPORT_LIMIT + 1),
                "--limit must be between 1 and 100",
            ),
            (
                selector_args(|args| args.agent = Some("gemini".to_string())),
                "agent import supports claude-code, codex, opencode, hermes, or pi; got 'gemini'",
            ),
            (
                selector_args(|args| {
                    args.all = false;
                    args.path = Some(PathBuf::from("/provider/root/x.jsonl"));
                    args.agent = Some("opencode".to_string());
                }),
                "opencode has no transcript file; use --session so Libra can run the trusted export bridge",
            ),
            (
                selector_args(|args| {
                    args.all = false;
                    args.session = Some("bad/id".to_string());
                }),
                "invalid provider session id (expected a safe Claude or Codex session identifier)",
            ),
            (
                selector_args(|args| {
                    args.all = false;
                    args.session = Some("legacy.identifier".to_string());
                    args.agent = Some("codex".to_string());
                }),
                "invalid codex session id (expected alphanumeric/dash/underscore, at most 64 characters)",
            ),
            (
                selector_args(|args| args.agent = Some("opencode".to_string())),
                "OpenCode batch discovery is unavailable; select a session explicitly with --session",
            ),
            (
                selector_args(|args| {
                    args.all = false;
                    args.since = Some("yesterday".to_string());
                }),
                "--since must be a valid RFC3339 timestamp",
            ),
        ];
        for (args, expected) in cases {
            let error = validate_discovery_selector(&args).expect_err(expected);
            assert_eq!(error.message(), expected);
            assert_eq!(error.stable_code(), StableErrorCode::CliInvalidArguments);
            assert_eq!(error.kind(), CliErrorKind::CommandUsage);
        }

        let oversized = format!("x{}\u{1b}[2J", "y".repeat(80));
        let error = validate_discovery_selector(&selector_args(|args| {
            args.agent = Some(oversized.clone());
        }))
        .expect_err("unsupported agent");
        assert_eq!(
            error.message(),
            format!(
                "agent import supports claude-code, codex, opencode, hermes, or pi; got 'x{}…'",
                "y".repeat(63)
            )
        );
        let error = validate_discovery_selector(&selector_args(|args| {
            args.agent = Some("bad\u{1b}[2Jslug".to_string());
        }))
        .expect_err("unsupported agent");
        assert_eq!(
            error.message(),
            "agent import supports claude-code, codex, opencode, hermes, or pi; got 'bad?[2Jslug'"
        );
    }

    #[test]
    fn discovery_rejections_render_fixed_actionable_messages() {
        for (reason, message, code, kind) in [
            (
                DiscoveryRejection::SelectorRejected,
                "agent import discovery rejected the supplied selector",
                StableErrorCode::CliInvalidArguments,
                CliErrorKind::CommandUsage,
            ),
            (
                DiscoveryRejection::AmbiguousSession,
                "the session id matches multiple providers; add --agent",
                StableErrorCode::CliInvalidArguments,
                CliErrorKind::CommandUsage,
            ),
            (
                DiscoveryRejection::SessionNotFound,
                "no authorized local transcript matched the session id; use --agent opencode for an export-only OpenCode session",
                StableErrorCode::CliInvalidTarget,
                CliErrorKind::Fatal,
            ),
            (
                DiscoveryRejection::CursorOutOfRange,
                "--cursor is outside the discovery result set",
                StableErrorCode::CliInvalidArguments,
                CliErrorKind::CommandUsage,
            ),
            (
                DiscoveryRejection::ClaudeProviderRoot,
                "Claude session discovery failed within its configured provider root",
                StableErrorCode::AgentTranscriptAuthorizationMissing,
                CliErrorKind::Fatal,
            ),
            (
                DiscoveryRejection::CodexProviderRoot,
                "Codex session discovery failed within its configured provider root",
                StableErrorCode::AgentTranscriptAuthorizationMissing,
                CliErrorKind::Fatal,
            ),
            (
                DiscoveryRejection::DeadlineExceeded,
                "agent import discovery exceeded its total execution deadline",
                StableErrorCode::AgentImportPartialBatch,
                CliErrorKind::Fatal,
            ),
        ] {
            let frame = serde_json::to_vec(&DiscoveryHelperResponse::Error { reason })
                .expect("serialize discovery rejection");
            let decoded = match serde_json::from_slice::<DiscoveryHelperResponse>(&frame)
                .expect("decode discovery rejection")
            {
                DiscoveryHelperResponse::Error { reason } => reason,
                DiscoveryHelperResponse::Ok { .. } => panic!("expected an error frame"),
            };
            let error = decoded.into_cli_error();
            assert_eq!(error.message(), message);
            assert_eq!(error.stable_code(), code);
            assert_eq!(error.kind(), kind);
        }
        assert_eq!(
            DiscoveryRejection::from_provider_error(
                &ImportError::DeadlineExceeded.into(),
                DiscoveryRejection::CodexProviderRoot,
            ),
            DiscoveryRejection::DeadlineExceeded
        );
        assert_eq!(
            DiscoveryRejection::from_provider_error(
                &anyhow::anyhow!("/private/provider/root: permission denied"),
                DiscoveryRejection::ClaudeProviderRoot,
            ),
            DiscoveryRejection::ClaudeProviderRoot
        );
    }

    #[test]
    fn configured_source_cap_cannot_exceed_adapter_hard_cap() {
        assert_eq!(effective_source_read_cap(1024), 1024);
        assert_eq!(
            effective_source_read_cap(TRANSCRIPT_READ_HARD_CAP_BYTES * 2),
            TRANSCRIPT_READ_HARD_CAP_BYTES
        );
    }

    /// A command deadline reached after the barrier UPSERT but before COMMIT
    /// must roll the whole transaction back. In particular, a retry must not
    /// overwrite another repair generation and cannot create import/session
    /// side effects merely by losing a SQLite writer slot late.
    #[tokio::test]
    async fn import_barrier_deadline_after_persist_preserves_existing_v2_recovery_state() {
        let dir = tempfile::tempdir().expect("create import barrier deadline fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import barrier deadline database");
        let scope = CaptureScope {
            repo_id: "import-barrier-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("import-barrier-deadline-workspace".to_string()),
            workspace_fence: Some(37),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                       'active', 'import-barrier-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live import barrier deadline workspace");
        let source_id = format!("source/hmac-v2/{}", "b".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-barrier-deadline-provider".to_string(),
            session_id: "claude_code__import_barrier_deadline".to_string(),
            source_kind: "file".to_string(),
            source_id: source_id.clone(),
            identity_schema_version: 2,
            content_digest: "import-barrier-deadline-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "not_retained".to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: source_id.clone(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"raw_persisted": false}),
            transcript_snapshot: None,
            turn_boundaries: std::collections::BTreeMap::new(),
            turns: Vec::new(),
        };
        let identity_id = import_identity_id(&request);
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, 'active', ?, '{}', '{}', 1, 1, 2,
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
        .expect("seed import session without side effects");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, observed_digest, next_ordinal, state, owner,
                lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, ?, ?, 2, ?, 0, 'committed', NULL,
                       NULL, 73, 1, 1, ?, ?, ?, ?, 'scoped')",
            [
                identity_id.clone().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                request.content_digest.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed committed import identity");
        let marker = ImportIndexRepairMarker {
            schema_version: IMPORT_INDEX_REPAIR_MARKER_SCHEMA_V2,
            owner: "prior-deadline-owner".to_string(),
            generation: "prior-deadline-generation".to_string(),
            identity_id: identity_id.clone(),
            agent_kind: request.agent_kind.as_db_str().to_string(),
            provider_session_id: request.provider_session_id.clone(),
            source_kind: request.source_kind.clone(),
            source_id: request.source_id.clone(),
            state: "repair_pending".to_string(),
            lease_expires_at: 0,
            created_at: 1,
            fence_token: Some(73),
            capture_scope: Some(scope.clone()),
        };
        persist_import_index_barrier(&conn, &request.session_id, &marker)
            .await
            .expect("seed existing V2 repair marker");
        let marker_before = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read seeded marker")
        .expect("seeded marker exists")
        .value;
        let identity_before = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, lease_expires_at, fence_token, last_error_code
                 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.clone().into()],
            ))
            .await
            .expect("read identity before deadline")
            .expect("seeded identity exists");
        let identity_before = (
            identity_before
                .try_get_by::<String, _>("state")
                .expect("decode identity state before deadline"),
            identity_before
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode identity owner before deadline"),
            identity_before
                .try_get_by::<Option<i64>, _>("lease_expires_at")
                .expect("decode identity lease before deadline"),
            identity_before
                .try_get_by::<Option<i64>, _>("fence_token")
                .expect("decode identity fence before deadline"),
            identity_before
                .try_get_by::<Option<String>, _>("last_error_code")
                .expect("decode identity error before deadline"),
        );
        let session_before = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, metadata_json, redaction_report, schema_version
                 FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read session before deadline")
            .expect("seeded session exists");
        let session_before = (
            session_before
                .try_get_by::<String, _>("state")
                .expect("decode session state before deadline"),
            session_before
                .try_get_by::<String, _>("metadata_json")
                .expect("decode session metadata before deadline"),
            session_before
                .try_get_by::<String, _>("redaction_report")
                .expect("decode session report before deadline"),
            session_before
                .try_get_by::<i64, _>("schema_version")
                .expect("decode session schema before deadline"),
        );

        let reset = test_support::install(test_support::ImportTestControls {
            index_barrier_persist_delay: Some(Duration::from_millis(80)),
            ..test_support::ImportTestControls::default()
        });
        let error = acquire_import_index_barrier(
            &conn,
            dir.path(),
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_millis(20))
                .expect("establish import barrier deadline"),
        )
        .await
        .expect_err("post-persist deadline must roll back barrier acquisition");
        drop(reset);
        assert!(
            error
                .downcast_ref::<ImportError>()
                .is_some_and(|error| { matches!(error, ImportError::DeadlineExceeded) }),
            "deadline classification must survive the post-persist rollback: {error:#}"
        );

        let marker_after = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read marker after deadline")
        .expect("existing marker must survive deadline")
        .value;
        assert_eq!(
            marker_after, marker_before,
            "deadline must not replace an existing V2 marker generation or value"
        );
        let identity_after = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, lease_expires_at, fence_token, last_error_code
                 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.into()],
            ))
            .await
            .expect("read identity after deadline")
            .expect("identity must remain");
        let identity_after = (
            identity_after
                .try_get_by::<String, _>("state")
                .expect("decode identity state after deadline"),
            identity_after
                .try_get_by::<Option<String>, _>("owner")
                .expect("decode identity owner after deadline"),
            identity_after
                .try_get_by::<Option<i64>, _>("lease_expires_at")
                .expect("decode identity lease after deadline"),
            identity_after
                .try_get_by::<Option<i64>, _>("fence_token")
                .expect("decode identity fence after deadline"),
            identity_after
                .try_get_by::<Option<String>, _>("last_error_code")
                .expect("decode identity error after deadline"),
        );
        let session_after = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, metadata_json, redaction_report, schema_version
                 FROM agent_session WHERE session_id = ?",
                [request.session_id.clone().into()],
            ))
            .await
            .expect("read session after deadline")
            .expect("session must remain");
        let session_after = (
            session_after
                .try_get_by::<String, _>("state")
                .expect("decode session state after deadline"),
            session_after
                .try_get_by::<String, _>("metadata_json")
                .expect("decode session metadata after deadline"),
            session_after
                .try_get_by::<String, _>("redaction_report")
                .expect("decode session report after deadline"),
            session_after
                .try_get_by::<i64, _>("schema_version")
                .expect("decode session schema after deadline"),
        );
        assert_eq!(
            identity_after, identity_before,
            "deadline acquisition must not mutate the import identity"
        );
        assert_eq!(
            session_after, session_before,
            "deadline acquisition must not mutate the import session"
        );
    }

    /// A normal import deadline must bound the actual file-backed SQLite
    /// writer acquisition, not merely an in-process delay seam. Releasing the
    /// competing writer afterwards must not let a cancelled acquisition wake
    /// up and publish its barrier late.
    #[tokio::test]
    async fn import_barrier_deadline_bounds_file_sqlite_writer_acquisition_without_delayed_marker()
    {
        let dir = tempfile::tempdir().expect("create import barrier lock fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import barrier lock database");
        let lock_conn = crate::internal::db::establish_connection(&db_path.to_string_lossy())
            .await
            .expect("open independent import barrier lock connection");
        let source_id = format!("source/hmac-v2/{}", "c".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-barrier-lock-provider".to_string(),
            session_id: "claude_code__import_barrier_lock".to_string(),
            source_kind: "file".to_string(),
            source_id: source_id.clone(),
            identity_schema_version: 2,
            content_digest: "import-barrier-lock-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "not_retained".to_string(),
            capture_scope: Some(CaptureScope {
                repo_id: "import-barrier-lock-repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            }),
            source_fingerprint: source_id,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"raw_persisted": false}),
            transcript_snapshot: None,
            turn_boundaries: std::collections::BTreeMap::new(),
            turns: Vec::new(),
        };
        let holder = crate::internal::db::begin_write_transaction(&lock_conn)
            .await
            .expect("hold file SQLite writer for import barrier acquisition");
        let started = Instant::now();
        let error = acquire_import_index_barrier_transaction(
            &conn,
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_millis(30))
                .expect("establish import barrier lock deadline"),
        )
        .await
        .expect_err("contended barrier acquisition must observe its deadline");
        assert!(
            error
                .downcast_ref::<ImportError>()
                .is_some_and(|error| matches!(error, ImportError::DeadlineExceeded)),
            "writer acquisition must preserve the typed deadline: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "writer acquisition ignored the short deadline"
        );
        holder
            .rollback()
            .await
            .expect("release file SQLite writer after import barrier deadline");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let marker = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read import barrier marker after cancelled acquisition");
        assert!(
            marker.is_none(),
            "a cancelled writer acquisition must not publish its barrier after the holder releases"
        );
    }

    /// A corrupt later checkpoint must reject the entire replay repair before
    /// it writes any index row for an earlier valid checkpoint. The durable
    /// barrier remains byte-for-byte intact so a healthy retry can own it.
    #[cfg(unix)]
    #[tokio::test]
    async fn import_index_repair_is_atomic_when_a_later_loose_target_is_not_zlib() {
        let dir = tempfile::tempdir().expect("create import index repair fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import index repair database");
        let scope = CaptureScope {
            repo_id: "import-index-repair-atomic-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("import-index-repair-atomic-workspace".to_string()),
            workspace_fence: Some(41),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                       'active', 'import-index-repair-atomic-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live import index repair workspace");
        crate::internal::config::ConfigKv::set_with_conn(
            &conn,
            "libra.repoid",
            &scope.repo_id,
            false,
        )
        .await
        .expect("pin import index repair fixture repository id");
        let source_id = format!("source/hmac-v2/{}", "d".repeat(64));
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-index-repair-atomic-provider".to_string(),
            session_id: "claude_code__import_repair_atomic".to_string(),
            source_kind: "file".to_string(),
            source_id: source_id.clone(),
            identity_schema_version: 2,
            content_digest: "import-index-repair-atomic-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "not_retained".to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: source_id,
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"raw_persisted": false}),
            transcript_snapshot: None,
            turn_boundaries: std::collections::BTreeMap::new(),
            turns: Vec::new(),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, 'active', ?, '{}', '{}', 1, 1, 2,
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
        let barrier = acquire_import_index_barrier(
            &conn,
            dir.path(),
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_secs(1))
                .expect("establish import index repair deadline"),
        )
        .await
        .expect("acquire live import index repair barrier");
        let marker_before = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read import index repair barrier")
        .expect("import index repair barrier exists")
        .value;

        let first = write_import_index_repair_checkpoint(
            dir.path(),
            "aa000000-0000-0000-0000-000000000001",
        );
        let second = write_import_index_repair_checkpoint(
            dir.path(),
            "bb000000-0000-0000-0000-000000000002",
        );
        for (checkpoint, created_at) in [(&first, 1_i64), (&second, 2_i64)] {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_checkpoint (
                    checkpoint_id, session_id, scope, parent_commit, tree_oid,
                    metadata_blob_oid, traces_commit, created_at
                 ) VALUES (?, ?, 'committed', NULL, ?, ?, ?, ?)",
                [
                    checkpoint.checkpoint_id.clone().into(),
                    request.session_id.clone().into(),
                    checkpoint.tree_oid.clone().into(),
                    checkpoint.metadata_blob_oid.clone().into(),
                    checkpoint.traces_commit.clone().into(),
                    created_at.into(),
                ],
            ))
            .await
            .expect("seed import index repair checkpoint");
        }
        for checkpoint in [&first, &second] {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced)
                 VALUES (?, 'commit', 1, ?, 1, 0)",
                [
                    checkpoint.traces_commit.clone().into(),
                    scope.repo_id.clone().into(),
                ],
            ))
            .await
            .expect("seed stale import index repair target");
        }
        let removed = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "DELETE FROM object_index WHERE repo_id = ? AND o_id IN (?, ?)",
                [
                    scope.repo_id.clone().into(),
                    first.traces_commit.clone().into(),
                    second.traces_commit.clone().into(),
                ],
            ))
            .await
            .expect("remove both import index repair targets");
        assert_eq!(
            removed.rows_affected(),
            2,
            "the repair fixture must start with both target rows absent"
        );

        let corrupt_target_path = dir
            .path()
            .join("objects")
            .join(&second.traces_commit[..2])
            .join(&second.traces_commit[2..]);
        std::fs::write(&corrupt_target_path, b"not a zlib loose object")
            .expect("corrupt second loose target after its valid fixture write");

        let error = super::super::doctor::repair_session_object_index(
            &conn,
            dir.path(),
            super::super::doctor::SessionObjectIndexRepairRequest {
                session_id: &request.session_id,
                marker_owner: &barrier.marker.owner,
                marker_generation: &barrier.marker.generation,
                agent_kind: request.agent_kind.as_db_str(),
                provider_session_id: &request.provider_session_id,
                capture_scope: &scope,
            },
        )
        .await
        .expect_err("a non-zlib later target must reject import index repair");
        assert!(
            format!("{error:#}").contains("integrity-check checkpoint object"),
            "repair must fail while validating the corrupt later target: {error:#}"
        );

        let indexed_after = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS count FROM object_index WHERE repo_id = ?",
                [scope.repo_id.clone().into()],
            ))
            .await
            .expect("count object-index rows after rejected repair")
            .expect("object-index count row exists")
            .try_get_by::<i64, _>("count")
            .expect("decode object-index count after rejected repair");
        assert_eq!(
            indexed_after, 0,
            "validation failure must not restore either target or partially index the first checkpoint"
        );
        let marker_after = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read barrier after rejected repair")
        .expect("rejected repair must retain its barrier")
        .value;
        assert_eq!(
            marker_after, marker_before,
            "rejected repair must preserve the durable barrier marker byte-for-byte"
        );
    }

    /// The object-index barrier is durable import state, not advisory local
    /// logging. A hook that loses its task-workspace lease either before or
    /// after a barrier mutation must not publish/retire it or downgrade the
    /// matching import identity while a newer workspace may be recovering the
    /// same session.
    #[tokio::test]
    async fn expired_workspace_scope_cannot_mutate_import_index_barrier_or_identity() {
        let dir = tempfile::tempdir().expect("create import barrier fixture");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(&db_path.to_string_lossy())
            .await
            .expect("create import barrier database");
        let scope = CaptureScope {
            repo_id: "import-barrier-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("import-barrier-scope-workspace".to_string()),
            workspace_fence: Some(31),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                       'active', 'import-barrier-scope-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                dir.path().to_string_lossy().into_owned().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live import barrier workspace");
        let request = ImportRequest {
            agent_kind: AgentKind::ClaudeCode,
            provider_session_id: "import-barrier-provider".to_string(),
            session_id: "claude_code__import_barrier_scope".to_string(),
            source_kind: "file".to_string(),
            source_id: format!("source/hmac-v2/{}", "a".repeat(64)),
            identity_schema_version: 2,
            content_digest: "import-barrier-digest".to_string(),
            started_at: 1,
            ended_at: 1,
            session_state: "active".to_string(),
            stopped_at: None,
            working_dir: dir.path().to_path_buf(),
            repository_identity: "import-barrier-repository".to_string(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: format!("source/hmac-v2/{}", "a".repeat(64)),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({
                "pipeline": "typed_allowlist",
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }),
            transcript_snapshot: None,
            turn_boundaries: std::collections::BTreeMap::new(),
            turns: Vec::new(),
        };
        // Each trigger expires the lease *inside* the transaction after the
        // named barrier mutation. This deterministically proves the final
        // commit fence rolls all preceding state back rather than relying on
        // a scheduler-dependent pause between DML and COMMIT.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE import_barrier_expiry_mode (mode TEXT NOT NULL)".to_string(),
        ))
        .await
        .expect("create import barrier expiry mode table");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO import_barrier_expiry_mode (mode) VALUES ('acquire')".to_string(),
        ))
        .await
        .expect("seed import barrier expiry mode");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_barrier_acquire
             AFTER INSERT ON metadata_kv
             WHEN NEW.scope = 'agent_import_index_repair'
               AND NEW.target = 'claude_code__import_barrier_scope'
               AND NEW.key = 'object-index-v1'
               AND (SELECT mode FROM import_barrier_expiry_mode LIMIT 1) = 'acquire'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'import-barrier-scope-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-acquire expiry trigger");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_barrier_pending
             AFTER UPDATE ON metadata_kv
             WHEN NEW.scope = 'agent_import_index_repair'
               AND NEW.target = 'claude_code__import_barrier_scope'
               AND NEW.key = 'object-index-v1'
               AND NEW.value <> OLD.value
               AND (SELECT mode FROM import_barrier_expiry_mode LIMIT 1) = 'pending'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'import-barrier-scope-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-pending expiry trigger");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_barrier_clear
             AFTER DELETE ON metadata_kv
             WHEN OLD.scope = 'agent_import_index_repair'
               AND OLD.target = 'claude_code__import_barrier_scope'
               AND OLD.key = 'object-index-v1'
               AND (SELECT mode FROM import_barrier_expiry_mode LIMIT 1) = 'clear'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'import-barrier-scope-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-clear expiry trigger");

        let error = acquire_import_index_barrier(
            &conn,
            dir.path(),
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_secs(1))
                .expect("establish scoped import barrier deadline"),
        )
        .await
        .expect_err("expiry after barrier persist must roll back acquisition");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert!(
            MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentImportIndexRepair,
                &request.session_id,
                IMPORT_INDEX_REPAIR_MARKER_KEY,
            )
            .await
            .expect("read post-acquire rollback marker")
            .is_none(),
            "post-acquire expiry must not publish a durable barrier"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-acquire expiry trigger must roll back with the barrier");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE import_barrier_expiry_mode SET mode = 'off'".to_string(),
        ))
        .await
        .expect("disable post-acquire expiry trigger");

        let barrier = acquire_import_index_barrier(
            &conn,
            dir.path(),
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_secs(1))
                .expect("establish live scoped import barrier deadline"),
        )
        .await
        .expect("acquire barrier while workspace lease is live");
        let marker_before = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read newly acquired barrier")
        .expect("barrier row exists")
        .value;
        let persisted_marker = parse_import_index_barrier_marker(&marker_before)
            .expect("decode scoped barrier marker");
        assert_eq!(persisted_marker.capture_scope.as_ref(), Some(&scope));

        let identity_id = import_identity_id(&request);
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, observed_digest, next_ordinal, state, owner,
                lease_expires_at, fence_token, created_at, updated_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', ?, ?, ?, 2, ?, 0, 'committed', NULL,
                       NULL, ?, 1, 1, ?, ?, ?, ?, 'scoped')",
            [
                identity_id.clone().into(),
                request.provider_session_id.clone().into(),
                request.source_kind.clone().into(),
                request.source_id.clone().into(),
                request.content_digest.clone().into(),
                41_i64.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed committed scoped import identity");

        let identity = ImportIdentityFence {
            identity_id: identity_id.clone(),
            fence_token: 41,
        };
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE import_barrier_expiry_mode SET mode = 'pending'".to_string(),
        ))
        .await
        .expect("enable post-pending expiry trigger");
        let error = set_import_index_barrier_pending(&conn, &barrier, Some(&identity))
            .await
            .expect_err("expiry after pending mutation must roll back barrier and identity");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_pending = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read post-pending rollback marker")
        .expect("post-pending expiry must retain barrier")
        .value;
        assert_eq!(
            marker_after_pending, marker_before,
            "post-pending expiry must roll the barrier marker back byte-for-byte"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-pending expiry trigger must roll back with the barrier");

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE import_barrier_expiry_mode SET mode = 'clear'".to_string(),
        ))
        .await
        .expect("enable post-clear expiry trigger");
        let error = clear_import_index_barrier(&conn, &barrier)
            .await
            .expect_err("expiry after barrier delete must roll back retirement");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_clear = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read post-clear rollback marker")
        .expect("post-clear expiry must retain barrier")
        .value;
        assert_eq!(
            marker_after_clear, marker_before,
            "post-clear expiry must roll the barrier marker back byte-for-byte"
        );
        let identity_after_post_mutation_expiry = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, last_error_code, fence_token FROM agent_import_identity
                 WHERE identity_id = ?",
                [identity_id.clone().into()],
            ))
            .await
            .expect("read identity after post-mutation expiry")
            .expect("post-pending expiry must retain identity");
        assert_eq!(
            identity_after_post_mutation_expiry
                .try_get_by::<String, _>("state")
                .expect("decode post-mutation identity state"),
            "committed",
            "post-pending expiry must roll back the import identity downgrade"
        );
        assert_eq!(
            identity_after_post_mutation_expiry
                .try_get_by::<Option<String>, _>("last_error_code")
                .expect("decode post-mutation identity error"),
            None,
            "post-pending expiry must not leave a partial error code"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-clear expiry trigger must roll back with the barrier");

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0
             WHERE workspace_id = 'import-barrier-scope-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after barrier acquisition");

        let error = acquire_import_index_barrier(
            &conn,
            dir.path(),
            &request,
            CaptureCommitDeadline::from_budget(Duration::from_secs(1))
                .expect("establish expired scoped import barrier deadline"),
        )
        .await
        .expect_err("expired scope must not replace the import barrier");
        assert!(format!("{error:#}").contains("workspace lease"));

        let error = super::super::doctor::repair_session_object_index(
            &conn,
            dir.path(),
            super::super::doctor::SessionObjectIndexRepairRequest {
                session_id: &request.session_id,
                marker_owner: &barrier.marker.owner,
                marker_generation: &barrier.marker.generation,
                agent_kind: request.agent_kind.as_db_str(),
                provider_session_id: &request.provider_session_id,
                capture_scope: &scope,
            },
        )
        .await
        .expect_err("expired scope must not run the import object-index repair helper");
        assert!(format!("{error:#}").contains("workspace lease"));

        let error = set_import_index_barrier_pending(&conn, &barrier, Some(&identity))
            .await
            .expect_err("expired scope must not mark the barrier or identity partial");
        assert!(format!("{error:#}").contains("workspace lease"));
        let error = clear_import_index_barrier(&conn, &barrier)
            .await
            .expect_err("expired scope must not retire the import barrier");
        assert!(format!("{error:#}").contains("workspace lease"));

        let marker_after = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentImportIndexRepair,
            &request.session_id,
            IMPORT_INDEX_REPAIR_MARKER_KEY,
        )
        .await
        .expect("read barrier after failed stale mutations")
        .expect("stale cleanup must not delete barrier")
        .value;
        assert_eq!(
            marker_after, marker_before,
            "post-expiry acquire/repair/pending/clear must leave the durable marker byte-identical"
        );
        let identity_row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, last_error_code, fence_token FROM agent_import_identity
                 WHERE identity_id = ?",
                [identity_id.into()],
            ))
            .await
            .expect("read identity after stale barrier mutations")
            .expect("seeded import identity remains");
        assert_eq!(
            identity_row
                .try_get_by::<String, _>("state")
                .expect("decode identity state"),
            "committed"
        );
        assert_eq!(
            identity_row
                .try_get_by::<Option<String>, _>("last_error_code")
                .expect("decode identity error"),
            None
        );
        assert_eq!(
            identity_row
                .try_get_by::<Option<i64>, _>("fence_token")
                .expect("decode identity fence"),
            Some(41)
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_wire_frame_fits_full_page_of_near_path_max_candidates() {
        use std::os::unix::ffi::OsStringExt;

        let long_path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'x'; 4095]));
        let response = DiscoveryHelperResponse::Ok {
            candidates: (0..MAX_IMPORT_LIMIT)
                .map(|index| DiscoveryCandidateWire {
                    kind: "claude-code".to_string(),
                    provider_session_id: format!("abcdef00-0000-0000-0000-{index:012}"),
                    path: Some(WirePath::from_path(&long_path)),
                })
                .collect(),
            next_cursor: Some(MAX_IMPORT_LIMIT),
            rejected: Vec::new(),
        };
        let frame = serde_json::to_vec(&response).unwrap();
        assert!(
            frame.len() as u64 <= IMPORT_DISCOVERY_HELPER_FRAME_CAP,
            "full discovery page encoded to {} bytes (cap {})",
            frame.len(),
            IMPORT_DISCOVERY_HELPER_FRAME_CAP
        );
        let decoded: DiscoveryHelperResponse = serde_json::from_slice(&frame).unwrap();
        let DiscoveryHelperResponse::Ok { candidates, .. } = decoded else {
            panic!("successful discovery frame changed variants")
        };
        assert_eq!(candidates.len(), MAX_IMPORT_LIMIT);
        assert_eq!(
            candidates[0].path.clone().unwrap().into_path_buf(),
            long_path
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interactive_consent_wait_observes_delayed_tty_input_and_absolute_timeout() {
        fn pty_pair() -> (i32, i32) {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: master/slave point to writable integers; null termios
            // and winsize request platform defaults for this test PTY.
            let result = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            assert_eq!(
                result,
                0,
                "open test PTY: {}",
                std::io::Error::last_os_error()
            );
            (master, slave)
        }

        let (master, slave) = pty_pair();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            let answer = b"yes\n";
            // SAFETY: master is the live PTY fd owned by this thread and the
            // byte slice is valid for the write duration.
            assert_eq!(
                unsafe { libc::write(master, answer.as_ptr().cast(), answer.len()) },
                answer.len() as isize
            );
            // SAFETY: this thread owns master and closes it once.
            unsafe { libc::close(master) };
        });
        assert!(
            wait_for_consent_fd(slave, Instant::now() + Duration::from_secs(1)).unwrap(),
            "delayed canonical TTY line never became readable"
        );
        writer.join().unwrap();
        // SAFETY: the test owns slave and closes it once.
        unsafe { libc::close(slave) };

        let (master, slave) = pty_pair();
        let started = Instant::now();
        assert!(
            !wait_for_consent_fd(slave, Instant::now() + Duration::from_millis(80)).unwrap(),
            "silent TTY bypassed the consent deadline"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        // SAFETY: the test owns both descriptors and closes each once.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(env)]
    fn claude_discovery_rejects_symlinked_project_directory_before_enumeration() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let _guard = TestHomeGuard::set(home.path());
        let repo = Path::new("/work/secure-project");
        let session_dir = claude_session_dir(repo).unwrap();
        std::fs::create_dir_all(session_dir.parent().unwrap()).unwrap();
        std::fs::write(
            outside
                .path()
                .join("abcdef00-0000-0000-0000-000000000001.jsonl"),
            b"private outside data\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), &session_dir).unwrap();

        let error =
            discover_claude(repo, None, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert!(
            error.to_string().contains("no-follow")
                || error
                    .to_string()
                    .contains("Too many levels of symbolic links"),
            "unexpected error: {error:#}"
        );
    }
}
