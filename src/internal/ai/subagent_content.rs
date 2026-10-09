//! Source-scoped subagent transcript capture (plan-20260713 M5 / DR-06).
//!
//! Provider files are discovered beneath a held, no-follow directory and are
//! converted to typed, allowlist-only projections before persistence.  The
//! checkpoint catalog row, source revision/current leaf, association row, and
//! traces ref CAS commit atomically through [`history::TracesTxnExtra`].

#[cfg(unix)]
use std::process::Stdio;
use std::{
    collections::HashMap,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(unix)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use tokio::io::AsyncWriteExt;

#[cfg(unix)]
use super::authorized_read::{
    CancellationSafeChild, StrictBoundedRead, configure_private_helper_process_group,
    read_async_strictly_bounded, read_strictly_bounded,
};
#[cfg(unix)]
use super::observed_agents::{
    claude_project_slug, claude_session_dir, open_file_beneath_pinned_provider_directory,
    open_provider_directory_for_discovery,
};
use super::{
    capture::snapshot::{CaptureSnapshot, CaptureSnapshotPolicy, CaptureSnapshotService},
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    history::{
        self, CheckpointCommitParams, CheckpointScope, HistoryManager, TracesCommitCtx,
        TracesInflightMarker, TracesTxnExtra,
    },
    hooks::runtime::{
        CaptureSourceCommitmentDomain, derive_capture_source_commitment_in_scope_until,
    },
    observed_agents::{
        ClaudeCodeObservedAgent, ExportAuthorized, MAX_REDACTION_MATCH_SAMPLES, RedactedBytes,
        RedactionReport, Redactor, TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource,
        claude_session_id_is_safe_path_component, normalize_claude_transcript,
        normalize_claude_transcript_until, parse_canon_value, redact_turns_with_report,
        safe_turn_projection,
    },
};
use crate::utils::client_storage::ClientStorage;

/// New content claims use a repository-keyed HMAC source key.  Schema V1
/// rows remain immutable/read-only proof: a live capture must never create a
/// parallel V2 history for a source that still has V1 recovery state.
const SUBAGENT_CONTENT_SCHEMA_VERSION_V1: i64 = 1;
pub const SUBAGENT_CONTENT_SCHEMA_VERSION: i64 = 2;
pub const SUBAGENT_DISCOVERY_HELPER_ARG: &str = "--libra-internal-agent-subagent-discovery-helper";
pub const SUBAGENT_DISCOVERY_HELPER_INPUT_CAP: u64 = 1024 * 1024;
// A discovery response starts with a small JSON metadata header followed by
// direct raw source segments. Sending sources as JSON/base64 would retain the
// pipe buffer and a second owned base64 String in the deadline-owning parent,
// exceeding ACF-03's 2.5x raw working-set budget.
const SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES: usize = 4;
const SUBAGENT_DISCOVERY_HELPER_HEADER_CAP: u64 = 64 * 1024;
pub const SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP: u64 = TRANSCRIPT_READ_HARD_CAP_BYTES
    + SUBAGENT_DISCOVERY_HELPER_HEADER_CAP
    + SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES as u64;
/// Private, killable helper for normalize/redact/project work over one held
/// child transcript. The parent never performs that CPU-heavy work while it
/// is answerable to a hook/import deadline.
pub const SUBAGENT_PROJECTION_HELPER_ARG: &str =
    "--libra-internal-agent-subagent-projection-helper";
const SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES: usize = 1 + 8;
pub const SUBAGENT_PROJECTION_HELPER_INPUT_CAP: u64 =
    TRANSCRIPT_READ_HARD_CAP_BYTES + SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES as u64;
// A projection may need modest JSON escaping/allowlist framing expansion, but
// the parent retains the native child buffer while it drains helper output.
// Keep proportional output below 1.5x input so the two buffers stay within
// ACF-03's 2.5x working-set budget (apart from the small bounded report/frame).
// Short, valid JSONL transcripts also need a fixed schema floor: their typed
// projection has object-key overhead that is not proportional to a tiny raw
// payload. The floor is fixed and bounded, never an input-controlled expansion.
const SUBAGENT_PROJECTION_OUTPUT_NUMERATOR: u64 = 3;
const SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR: u64 = 2;
const SUBAGENT_PROJECTION_TRANSCRIPT_CAP: u64 = TRANSCRIPT_READ_HARD_CAP_BYTES
    * SUBAGENT_PROJECTION_OUTPUT_NUMERATOR
    / SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR;
const SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP: u64 = 64 * 1024;
const SUBAGENT_PROJECTION_REPORT_CAP: u64 = 64 * 1024;
const SUBAGENT_PROJECTION_FRAME_HEADER_BYTES: usize = 1 + 1 + 8 + 8 + 4 + 32;
pub const SUBAGENT_PROJECTION_HELPER_OUTPUT_CAP: u64 = SUBAGENT_PROJECTION_TRANSCRIPT_CAP
    + SUBAGENT_PROJECTION_REPORT_CAP
    + SUBAGENT_PROJECTION_FRAME_HEADER_BYTES as u64;
const SUBAGENT_PROJECTION_COMPLETE: u8 = 0;
const SUBAGENT_PROJECTION_FAILED: u8 = 1;
const SUBAGENT_CONTENT_LEASE_MS: i64 = 60_000;
const MAX_SUBAGENT_DIRECTORY_ENTRIES: usize = 2_048;
pub(crate) const MAX_SUBAGENT_SOURCES_PER_CAPTURE: usize = 16;
const SUBAGENT_PARENT_PERSISTENCE_RESERVE: Duration = Duration::from_secs(20);
const SUBAGENT_RESERVATION_RELEASE_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
#[error("subagent discovery exhausted its parent-preservation deadline")]
pub(crate) struct SubagentDiscoveryDeadline;

#[derive(Debug, thiserror::Error)]
#[error("subagent content capture exceeded its command deadline")]
pub(crate) struct SubagentCaptureDeadline;

/// One provider-authorized child transcript. Fields remain private so callers
/// cannot forge the discovery proof with arbitrary bytes.
#[derive(Clone)]
pub(crate) struct DiscoveredSubagentContent {
    provider_kind: String,
    /// A private helper-side/delivery proof only. Before persistence this is
    /// replaced with a repository-keyed V2 commitment. It must never reach a
    /// catalog claim, revision, checkpoint metadata, or trace object.
    source_key: String,
    /// Exact V1 lookup key retained only while the scoped writer decides
    /// whether a legacy immutable proof blocks V2 materialization.
    legacy_source_key: Option<String>,
    bytes: Vec<u8>,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
}

/// Child transcript bytes are native/raw at discovery time.  Keep `Debug`
/// content-free so an error or span cannot stringify a full provider file.
impl fmt::Debug for DiscoveredSubagentContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredSubagentContent")
            .field("provider_kind", &self.provider_kind)
            .field("source_key_len", &self.source_key.len())
            .field(
                "legacy_source_key_present",
                &self.legacy_source_key.is_some(),
            )
            .field("byte_len", &self.bytes.len())
            .field("malformed_lines", &self.malformed_lines)
            .field("partial", &self.partial)
            .field(
                "stable_subagent_id_present",
                &self.stable_subagent_id.is_some(),
            )
            .finish()
    }
}

/// Bounded discovery result.  `bytes_read` is charged to the historical
/// import command's cumulative input budget even if later persistence fails.
#[derive(Debug, Clone, Default)]
pub(crate) struct SubagentDiscovery {
    pub sources: Vec<DiscoveredSubagentContent>,
    pub bytes_read: u64,
    /// A platform capability warning is non-fatal for the parent checkpoint.
    /// It is diagnostic only: absence of a supported discovery mechanism does
    /// not prove that child evidence exists or is incomplete.
    pub warning: Option<String>,
    /// Child evidence could not be validated within its reserved slice of the
    /// command deadline. The independently valid parent remains importable,
    /// but its result must be reported as partial.
    pub incomplete: bool,
}

impl SubagentDiscovery {
    pub(crate) fn partial_source_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.partial)
            .count()
            .saturating_add(usize::from(self.incomplete))
    }

    pub(crate) fn from_deadline_error(error: &anyhow::Error) -> Option<Self> {
        error
            .downcast_ref::<SubagentDiscoveryDeadline>()
            .map(|_| Self {
                warning: Some(
                    "subagent discovery exceeded its reserved time; parent evidence was preserved as partial"
                        .to_string(),
                ),
                incomplete: true,
                ..Self::default()
            })
    }
}

impl DiscoveredSubagentContent {
    /// Consume this securely discovered child source at the one boundary
    /// where parent extraction needs its content. The runtime receives only
    /// the resulting snapshot, whose transcript is already redacted; it can
    /// never borrow the native child bytes directly.
    pub(crate) fn into_parent_extraction_snapshot(
        self,
        parent_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        let agent_kind = self.provider_kind.clone();
        let auth = ExportAuthorized::issue_discovered_subagent(
            &agent_kind,
            parent_session_id,
            &self.bytes,
        );
        CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes {
                bytes: self.bytes,
                auth,
            },
            &agent_kind,
            parent_session_id,
            policy,
        )
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        provider_kind: &str,
        source_key: &str,
        bytes: &[u8],
        stable_subagent_id: Option<&str>,
    ) -> Self {
        Self {
            provider_kind: provider_kind.to_string(),
            source_key: source_key.to_string(),
            legacy_source_key: None,
            bytes: bytes.to_vec(),
            malformed_lines: 0,
            partial: false,
            stable_subagent_id: stable_subagent_id.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentCaptureSummary {
    pub discovered: usize,
    pub checkpoints_written: usize,
    pub skipped_unchanged: usize,
    pub skipped_inflight: usize,
    pub partial_sources: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("subagent content capture failed after durable progress: {source:#}")]
pub(crate) struct SubagentCaptureProgressError {
    summary: SubagentCaptureSummary,
    #[source]
    source: anyhow::Error,
}

impl SubagentCaptureProgressError {
    pub(crate) fn summary(&self) -> &SubagentCaptureSummary {
        &self.summary
    }

    pub(crate) fn is_deadline_exhausted(&self) -> bool {
        self.source
            .chain()
            .any(|cause| cause.is::<SubagentCaptureDeadline>())
    }
}

/// Whether a child-content write stopped only because it reached its bounded
/// child slice. The parent can safely continue as partial in this case; other
/// child failures retain their fail-closed behavior.
pub(crate) fn capture_deadline_exhausted(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SubagentCaptureProgressError>()
        .is_some_and(SubagentCaptureProgressError::is_deadline_exhausted)
        || error.is::<SubagentCaptureDeadline>()
}

fn ensure_before_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(SubagentCaptureDeadline.into());
    }
    Ok(())
}

fn normalize_subagent_marker_deadline(error: anyhow::Error) -> anyhow::Error {
    if error
        .chain()
        .any(|cause| cause.is::<crate::internal::ai::traces::TracesMarkerDeadlineExceeded>())
    {
        return SubagentCaptureDeadline.into();
    }
    error
}

/// Releasing a reservation is the narrow cleanup exception to capture's
/// deadline: it can only clear the caller's already-fenced provisional row,
/// never create or refresh a claim/link/marker. Give that recovery write one
/// short paired deadline after expiry so a failed capture does not strand its
/// own durable lease indefinitely. A live foreground deadline remains a
/// ceiling in both clock domains: its SQLite authorization must never be
/// silently replaced by a fresh recovery wall-clock deadline.
fn subagent_cleanup_deadline(deadline: CaptureCommitDeadline) -> Result<CaptureCommitDeadline> {
    if deadline.monotonic() <= Instant::now() {
        return CaptureCommitDeadline::from_budget(SUBAGENT_RESERVATION_RELEASE_GRACE)
            .context("establish subagent reservation-release recovery deadline");
    }

    let recovery_deadline = CaptureCommitDeadline::from_budget(SUBAGENT_RESERVATION_RELEASE_GRACE)
        .context("establish subagent reservation-release recovery deadline")?;
    Ok(CaptureCommitDeadline::from_established_pair(
        deadline.monotonic().min(recovery_deadline.monotonic()),
        deadline
            .sqlite_not_after_millis()
            .min(recovery_deadline.sqlite_not_after_millis()),
    ))
}

fn ensure_before_discovery_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(SubagentDiscoveryDeadline.into());
    }
    Ok(())
}

/// Reserve enough of the caller's absolute deadline to persist the parent
/// checkpoint when child discovery or parent-side validation is slow.
pub(crate) fn discovery_deadline_preserving_parent(command_deadline: Instant) -> Result<Instant> {
    let now = Instant::now();
    if now >= command_deadline {
        return Err(SubagentDiscoveryDeadline.into());
    }
    let remaining = command_deadline.saturating_duration_since(now);
    let reserve = SUBAGENT_PARENT_PERSISTENCE_RESERVE.min(remaining / 2);
    now.checked_add(remaining.saturating_sub(reserve))
        .context("compute parent-preserving subagent discovery deadline")
}

#[cfg(test)]
async fn subagent_discovery_test_pause_before_parent_validation(deadline: Instant) {
    let Some(delay) = TEST_SUBAGENT_PARENT_VALIDATION_DELAY
        .try_with(|configured| *configured)
        .ok()
        .flatten()
    else {
        return;
    };
    // When the test delay meets or exceeds the discovery window, sleep only
    // until that window so the following deadline check can preserve the
    // parent with its reserved persistence budget intact.
    let until = deadline.saturating_duration_since(Instant::now());
    tokio::time::sleep(delay.min(until)).await;
}

fn subagent_source_completeness(bytes: &[u8], deadline: Option<Instant>) -> Result<(usize, bool)> {
    let mut malformed_lines = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        ensure_before_deadline(deadline)?;
        if !line.iter().all(u8::is_ascii_whitespace) && parse_canon_value(line).is_err() {
            malformed_lines = malformed_lines.saturating_add(1);
        }
    }
    let turns = match deadline {
        Some(deadline) => normalize_claude_transcript_until(bytes, deadline)
            .context("subagent content normalization exceeded its command deadline")?,
        None => normalize_claude_transcript(bytes),
    };
    ensure_before_deadline(deadline)?;
    let partial = malformed_lines > 0
        || turns.is_empty()
        || turns
            .iter()
            .any(|turn| turn.completeness.as_db_str() == "incomplete");
    Ok((malformed_lines, partial))
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_CONTENT_FAILPOINT: Option<&'static str>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_PARENT_VALIDATION_DELAY: Option<Duration>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM: Option<PathBuf>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP: Option<u64>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM: Option<PathBuf>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_DISCOVERY_OVERRIDE: Option<SubagentDiscovery>;
}

// Test-only observation point at the final history append boundary. It proves
// that the compatibility wrapper's bounded fallback is propagated past
// preparation and reservation into the actual durable checkpoint sink.
#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_FINAL_APPEND_DEADLINE: Arc<std::sync::Mutex<Option<Option<CaptureCommitDeadline>>>>;
}

// Library unit fixtures intentionally run without the CLI-owned private
// checkpoint helper. Preserve their established synchronous fixture mode
// unless a regression explicitly asks to exercise the bounded append path.
#[cfg(test)]
tokio::task_local! {
    static TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE: bool;
}

#[cfg(test)]
pub(crate) async fn with_subagent_parent_validation_delay<F>(
    delay: Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_PARENT_VALIDATION_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
pub(crate) async fn with_subagent_discovery_helper_program<F>(
    program: PathBuf,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM
        .scope(Some(program), future)
        .await
}

#[cfg(test)]
async fn with_subagent_discovery_helper_output_cap<F>(cap: u64, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
        .scope(Some(cap), future)
        .await
}

#[cfg(test)]
async fn with_subagent_projection_helper_program<F>(program: PathBuf, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM
        .scope(Some(program), future)
        .await
}

#[cfg(test)]
pub(crate) async fn with_subagent_discovery_override<F>(
    discovery: SubagentDiscovery,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_SUBAGENT_DISCOVERY_OVERRIDE
        .scope(Some(discovery), future)
        .await
}

#[cfg(test)]
fn observe_subagent_final_append_deadline(deadline: Option<CaptureCommitDeadline>) {
    let _ = TEST_SUBAGENT_FINAL_APPEND_DEADLINE.try_with(|observed| {
        *observed
            .lock()
            .expect("lock final subagent append deadline observation") = Some(deadline);
    });
}

#[cfg(not(test))]
fn observe_subagent_final_append_deadline(_deadline: Option<CaptureCommitDeadline>) {}

#[cfg(test)]
fn final_subagent_append_deadline(
    caller_deadline: Option<CaptureCommitDeadline>,
    effective_deadline: Option<CaptureCommitDeadline>,
) -> Option<CaptureCommitDeadline> {
    let force_bounded = TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE
        .try_with(|force| *force)
        .unwrap_or(false);
    if caller_deadline.is_none()
        && crate::internal::ai::authorized_read::helper_program().is_none()
        && !force_bounded
    {
        return None;
    }
    effective_deadline
}

#[cfg(not(test))]
fn final_subagent_append_deadline(
    _caller_deadline: Option<CaptureCommitDeadline>,
    effective_deadline: Option<CaptureCommitDeadline>,
) -> Option<CaptureCommitDeadline> {
    effective_deadline
}

fn subagent_content_test_failpoint(_stage: &str) -> Result<()> {
    #[cfg(test)]
    if TEST_SUBAGENT_CONTENT_FAILPOINT
        .try_with(|configured| {
            configured.is_some_and(|configured| {
                configured.split(',').any(|configured| configured == _stage)
            })
        })
        .unwrap_or(false)
    {
        bail!("injected subagent content failure at {_stage}");
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct SubagentDiscoveryHelperRequest {
    cwd_base64: String,
    provider_session_id: String,
    remaining_ms: u64,
    byte_budget: u64,
    source_limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveredSubagentContentWire {
    provider_kind: String,
    source_key: String,
    byte_len: u64,
    malformed_lines: usize,
    partial: bool,
    stable_subagent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum SubagentDiscoveryHelperResponse {
    Ok {
        sources: Vec<DiscoveredSubagentContentWire>,
        bytes_read: u64,
        warning: Option<String>,
        incomplete: bool,
    },
    Error {
        deadline_exceeded: bool,
    },
}

#[cfg(unix)]
fn encode_helper_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    STANDARD.encode(path.as_os_str().as_bytes())
}

#[cfg(unix)]
fn decode_helper_path(encoded: &str) -> Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let bytes = STANDARD
        .decode(encoded)
        .context("decode subagent discovery helper path")?;
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

/// Resolve the registered Libra executable for every private child-content
/// helper. Never infer a sibling named `libra`: managed hooks may be installed
/// under a renamed `--binary-path`, and an embedded host must fail closed.
fn helper_program() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(Some(program)) =
        TEST_SUBAGENT_DISCOVERY_HELPER_PROGRAM.try_with(|program| program.clone())
    {
        return Some(program);
    }
    crate::internal::ai::authorized_read::helper_program()
}

fn subagent_discovery_helper_output_cap() -> u64 {
    #[cfg(test)]
    if let Ok(Some(cap)) = TEST_SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP.try_with(|cap| *cap) {
        return cap;
    }
    SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
}

/// Resolve the main Libra executable which owns the projection helper
/// entrypoint. Unlike the historical discovery helper, this shares the
/// process-registered program fact so a supported `--binary-path` rename is
/// still safe and an embedded host never receives a private argv token.
fn projection_helper_program() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(Some(program)) = TEST_SUBAGENT_PROJECTION_HELPER_PROGRAM.try_with(Clone::clone) {
        return Some(program);
    }
    crate::internal::ai::authorized_read::helper_program()
}

fn subagent_projection_transcript_cap(raw_bytes: u64) -> u64 {
    let proportional_cap = raw_bytes
        .saturating_mul(SUBAGENT_PROJECTION_OUTPUT_NUMERATOR)
        .saturating_add(SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR.saturating_sub(1))
        / SUBAGENT_PROJECTION_OUTPUT_DENOMINATOR;
    proportional_cap.clamp(
        SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP,
        SUBAGENT_PROJECTION_TRANSCRIPT_CAP,
    )
}

fn subagent_projection_response_cap(raw_bytes: u64) -> u64 {
    subagent_projection_transcript_cap(raw_bytes)
        .saturating_add(SUBAGENT_PROJECTION_REPORT_CAP)
        .saturating_add(SUBAGENT_PROJECTION_FRAME_HEADER_BYTES as u64)
}

fn helper_unavailable_discovery() -> SubagentDiscovery {
    SubagentDiscovery {
        warning: Some(
            "killable subagent content discovery is unavailable in this executable".to_string(),
        ),
        ..SubagentDiscovery::default()
    }
}

/// Private helper entry: performs every potentially blocking provider-filesystem
/// operation outside the hook/import Tokio process. The parent owns the actual
/// kill deadline; this inner deadline also keeps ordinary local work bounded.
#[doc(hidden)]
pub fn run_subagent_discovery_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: SubagentDiscoveryHelperRequest =
        serde_json::from_slice(input).context("decode subagent discovery helper request")?;
    #[cfg(not(unix))]
    let _ = &request;
    #[cfg(not(unix))]
    let discovery = SubagentDiscovery {
        sources: Vec::new(),
        bytes_read: 0,
        warning: Some("secure subagent content discovery is unavailable on this platform".into()),
        incomplete: false,
    };
    #[cfg(unix)]
    let discovery = {
        let cwd = decode_helper_path(&request.cwd_base64)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(request.remaining_ms.max(1)))
            .context("compute subagent discovery helper deadline")?;
        match discover_claude_subagent_contents(
            &cwd,
            &request.provider_session_id,
            Some(deadline),
            request.byte_budget,
            request.source_limit,
        ) {
            Ok(discovery) => discovery,
            Err(_) => {
                return encode_subagent_discovery_helper_error(Instant::now() >= deadline);
            }
        }
    };
    encode_subagent_discovery_helper_success(discovery)
}

/// Encode a short JSON metadata header followed by direct raw source segments.
/// The header never carries provider-native bytes, while the raw tail avoids
/// retaining a second base64 expansion in the deadline-owning parent.
fn encode_subagent_discovery_helper_success(discovery: SubagentDiscovery) -> Result<Vec<u8>> {
    let raw_bytes = discovery.sources.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(u64::try_from(source.bytes.len()).map_err(|error| {
                anyhow!("subagent discovery helper source size overflow: {error}")
            })?)
            .context("subagent discovery helper source set size overflow")
    })?;
    if raw_bytes != discovery.bytes_read || raw_bytes > TRANSCRIPT_READ_HARD_CAP_BYTES {
        bail!("subagent discovery helper produced an invalid bounded source set");
    }
    let response = SubagentDiscoveryHelperResponse::Ok {
        sources: discovery
            .sources
            .iter()
            .map(|source| {
                Ok(DiscoveredSubagentContentWire {
                    provider_kind: source.provider_kind.clone(),
                    source_key: source.source_key.clone(),
                    byte_len: u64::try_from(source.bytes.len())
                        .context("subagent discovery helper source size exceeds wire range")?,
                    malformed_lines: source.malformed_lines,
                    partial: source.partial,
                    stable_subagent_id: source.stable_subagent_id.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        bytes_read: discovery.bytes_read,
        warning: discovery.warning,
        incomplete: discovery.incomplete,
    };
    let mut frame = encode_subagent_discovery_helper_header(&response)?;
    let raw_capacity = usize::try_from(raw_bytes)
        .context("subagent discovery helper source set exceeds addressable memory")?;
    frame
        .try_reserve_exact(raw_capacity)
        .context("allocate bounded subagent discovery helper response")?;
    for source in discovery.sources {
        frame.extend_from_slice(&source.bytes);
    }
    if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP {
        bail!("subagent discovery helper response exceeds its bounded output cap");
    }
    Ok(frame)
}

fn encode_subagent_discovery_helper_error(deadline_exceeded: bool) -> Result<Vec<u8>> {
    encode_subagent_discovery_helper_header(&SubagentDiscoveryHelperResponse::Error {
        deadline_exceeded,
    })
}

fn encode_subagent_discovery_helper_header(
    response: &SubagentDiscoveryHelperResponse,
) -> Result<Vec<u8>> {
    let header =
        serde_json::to_vec(response).context("encode subagent discovery helper metadata header")?;
    let header_len = u64::try_from(header.len())
        .context("subagent discovery helper metadata header length overflow")?;
    if header_len > SUBAGENT_DISCOVERY_HELPER_HEADER_CAP {
        bail!("subagent discovery helper metadata header exceeds its bounded cap");
    }
    let header_len = u32::try_from(header.len())
        .context("subagent discovery helper metadata header exceeds wire range")?;
    let capacity = SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
        .checked_add(header.len())
        .context("subagent discovery helper metadata frame length overflow")?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(capacity)
        .context("allocate bounded subagent discovery helper metadata frame")?;
    frame.extend_from_slice(&header_len.to_le_bytes());
    frame.extend_from_slice(&header);
    Ok(frame)
}

fn decode_subagent_discovery_helper_frame(
    output: Vec<u8>,
    byte_budget: u64,
    source_limit: usize,
    deadline: Instant,
) -> Result<SubagentDiscovery> {
    if output.len() < SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES {
        bail!("bounded subagent discovery helper returned an invalid response");
    }
    let header_len = u32::from_le_bytes([output[0], output[1], output[2], output[3]]) as usize;
    if u64::try_from(header_len)
        .context("bounded subagent discovery helper metadata header length overflow")?
        > SUBAGENT_DISCOVERY_HELPER_HEADER_CAP
    {
        bail!("bounded subagent discovery helper returned an invalid response");
    }
    let header_end = SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
        .checked_add(header_len)
        .filter(|end| *end <= output.len())
        .ok_or_else(|| anyhow!("bounded subagent discovery helper returned an invalid response"))?;
    let response: SubagentDiscoveryHelperResponse =
        serde_json::from_slice(&output[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end])
            .context("decode bounded subagent discovery metadata header")?;
    ensure_before_discovery_deadline(deadline)?;
    match response {
        SubagentDiscoveryHelperResponse::Ok {
            sources,
            bytes_read,
            warning,
            incomplete,
        } => {
            if sources.len() > source_limit
                || warning.as_ref().is_some_and(|value| value.len() > 4_096)
            {
                bail!("bounded subagent discovery helper exceeded its response limits");
            }
            let raw_bytes = sources.iter().try_fold(0_u64, |total, source| {
                total
                    .checked_add(source.byte_len)
                    .context("bounded subagent discovery helper byte count overflow")
            })?;
            let raw_end = header_end
                .checked_add(usize::try_from(raw_bytes).context(
                    "bounded subagent discovery helper raw frame exceeds addressable memory",
                )?)
                .filter(|end| *end == output.len())
                .ok_or_else(|| {
                    anyhow!("bounded subagent discovery helper returned an invalid response")
                })?;
            debug_assert_eq!(raw_end, output.len());
            if raw_bytes != bytes_read
                || raw_bytes > byte_budget.min(TRANSCRIPT_READ_HARD_CAP_BYTES)
            {
                bail!("bounded subagent discovery helper returned an inconsistent byte count");
            }
            let mut decoded = Vec::with_capacity(sources.len());
            let mut offset = header_end;
            for source in sources {
                ensure_before_discovery_deadline(deadline)?;
                if source.provider_kind != "claude_code" || source.stable_subagent_id.is_some() {
                    bail!("bounded subagent discovery helper returned invalid Claude attribution");
                }
                validate_source_identity(&source.provider_kind, &source.source_key)?;
                let byte_len = usize::try_from(source.byte_len).context(
                    "bounded subagent discovery helper raw source exceeds addressable memory",
                )?;
                let end = offset
                    .checked_add(byte_len)
                    .filter(|end| *end <= output.len())
                    .ok_or_else(|| {
                        anyhow!("bounded subagent discovery helper returned an invalid response")
                    })?;
                let max_malformed_lines = byte_len.saturating_add(1);
                if source.malformed_lines > max_malformed_lines
                    || (source.malformed_lines > 0 && !source.partial)
                    || (byte_len == 0 && !source.partial)
                {
                    bail!(
                        "bounded subagent discovery helper returned inconsistent completeness metadata"
                    );
                }
                // Keep one bounded raw pipe frame while copying each segment
                // into its durable discovery owner. At most 16 MiB of output
                // plus 16 MiB of source Vecs coexist, below ACF-03's 2.5x cap.
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(byte_len)
                    .context("allocate bounded subagent transcript bytes")?;
                bytes.extend_from_slice(&output[offset..end]);
                offset = end;
                decoded.push(DiscoveredSubagentContent {
                    provider_kind: source.provider_kind,
                    source_key: source.source_key,
                    legacy_source_key: None,
                    bytes,
                    malformed_lines: source.malformed_lines,
                    partial: source.partial,
                    stable_subagent_id: None,
                });
                ensure_before_discovery_deadline(deadline)?;
            }
            Ok(SubagentDiscovery {
                sources: decoded,
                bytes_read,
                warning,
                incomplete,
            })
        }
        SubagentDiscoveryHelperResponse::Error { deadline_exceeded } => {
            if header_end != output.len() {
                bail!("bounded subagent discovery helper returned an invalid response");
            }
            if deadline_exceeded {
                return Err(SubagentDiscoveryDeadline.into());
            }
            bail!("bounded subagent discovery helper reported a safe failure")
        }
    }
}

/// Killable async boundary used by every production live/import caller.
pub(crate) async fn discover_claude_subagent_contents_bounded(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Instant,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    #[cfg(test)]
    if let Ok(Some(discovery)) = TEST_SUBAGENT_DISCOVERY_OVERRIDE.try_with(|value| value.clone()) {
        return Ok(discovery);
    }
    #[cfg(not(unix))]
    return discover_claude_subagent_contents(
        cwd,
        provider_session_id,
        Some(deadline),
        byte_budget,
        source_limit,
    );

    #[cfg(unix)]
    {
        ensure_before_discovery_deadline(deadline)?;
        let Some(helper_program) = helper_program() else {
            // An embedded host cannot establish that it understands Libra's
            // private helper argv. Fail closed for child content: blocking
            // provider files must never be opened in the async host process
            // because they cannot be killed when the command deadline expires.
            return Ok(helper_unavailable_discovery());
        };
        let remaining_ms = u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        let request = SubagentDiscoveryHelperRequest {
            cwd_base64: encode_helper_path(cwd),
            provider_session_id: provider_session_id.to_string(),
            remaining_ms,
            byte_budget,
            source_limit,
        };
        let frame =
            serde_json::to_vec(&request).context("encode bounded subagent discovery request")?;
        if frame.len() as u64 > SUBAGENT_DISCOVERY_HELPER_INPUT_CAP {
            bail!("bounded subagent discovery request exceeds its internal frame limit");
        }
        let mut command = tokio::process::Command::new(helper_program);
        command
            .arg(SUBAGENT_DISCOVERY_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        let child = command
            .spawn()
            .context("start bounded subagent discovery helper")?;
        let mut child = CancellationSafeChild::new_process_group(child);
        let Some(mut stdin) = child.child_mut().and_then(|child| child.stdin.take()) else {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper has no stdin pipe");
        };
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper has no stdout pipe");
        };
        let output_cap = subagent_discovery_helper_output_cap();
        let mut output_task =
            tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
        child.register_abort_on_cancel(&output_task);
        let mut request_task = tokio::spawn(async move {
            stdin.write_all(&frame).await?;
            stdin.shutdown().await
        });
        child.register_abort_on_cancel(&request_task);
        let request_result: Result<()> = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut request_task,
        )
        .await
        {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(error).context("send bounded subagent discovery request"),
            Ok(Err(error)) => Err(anyhow!(
                "bounded subagent discovery request task failed: {error}"
            )),
            Err(_) => Err(SubagentDiscoveryDeadline.into()),
        };
        if let Err(error) = request_result {
            child.terminate_and_reap();
            return Err(error);
        }
        drop(request_task);

        // A discovery helper can fork a descendant that inherits both raw
        // source pipes. Drain stdout before `wait()`, so deadline/Drop still
        // owns an unreaped group leader and can kill that descendant safely.
        let output = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut output_task,
        )
        .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                bail!("bounded subagent discovery helper returned an invalid response")
            }
            Err(_) => {
                output_task.abort();
                child.terminate_and_reap();
                return Err(SubagentDiscoveryDeadline.into());
            }
        };
        if output.len() as u64 > output_cap {
            child.terminate_and_reap();
            bail!("bounded subagent discovery helper returned an invalid response");
        }
        let status_result = match child.child_mut() {
            Some(child_process) => {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    child_process.wait(),
                )
                .await
                {
                    Ok(Ok(status)) => Ok(status),
                    Ok(Err(error)) => {
                        Err(anyhow!(error).context("wait for bounded subagent discovery helper"))
                    }
                    Err(_) => Err(SubagentDiscoveryDeadline.into()),
                }
            }
            None => Err(anyhow!("bounded subagent discovery helper was unavailable")),
        };
        let status = match status_result {
            Ok(status) => {
                // stdout reached EOF before leader reaping, so no later path
                // can signal a recycled PGID.
                child.disarm_child_after_wait();
                status
            }
            Err(error) => {
                child.terminate_and_reap();
                return Err(error);
            }
        };
        child.finish();
        if !status.success() {
            bail!("bounded subagent discovery helper returned an invalid response");
        }
        #[cfg(test)]
        subagent_discovery_test_pause_before_parent_validation(deadline).await;
        ensure_before_discovery_deadline(deadline)?;
        decode_subagent_discovery_helper_frame(output, byte_budget, source_limit, deadline)
    }
}

fn validate_source_identity(provider_kind: &str, source_key: &str) -> Result<()> {
    if provider_kind.is_empty() || provider_kind.len() > 64 {
        bail!("subagent provider kind is empty or exceeds 64 bytes");
    }
    if source_key.is_empty() || source_key.len() > 4_096 {
        bail!("subagent source key is empty or exceeds 4096 bytes");
    }
    let path = Path::new(source_key);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("subagent source key must be provider-root-relative and normalized");
    }
    Ok(())
}

fn source_key_for_incarnation(source_key: &str, namespace: Option<&str>) -> Result<String> {
    let Some(namespace) = namespace else {
        return Ok(source_key.to_string());
    };
    if namespace.len() != 32 || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("agent capture incarnation namespace is invalid; run `libra agent doctor`");
    }
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-source-incarnation-v1");
    for value in [namespace.as_bytes(), source_key.as_bytes()] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    Ok(format!("source/sha256/{}", hex::encode(digest.finalize())))
}

fn update_subagent_source_preimage(digest: &mut Sha256, value: &[u8]) -> Result<()> {
    let length =
        u64::try_from(value.len()).context("subagent source identity field exceeds u64")?;
    digest.update(length.to_be_bytes());
    digest.update(value);
    Ok(())
}

/// Build a fixed, transient proof from helper-authorized source facts. It is
/// deliberately not a durable key: only the scoped repository HMAC below may
/// enter claims, revisions, checkpoint metadata, or traces.
fn subagent_source_preimage(
    provider_kind: &str,
    source_key: &str,
    capture_incarnation: Option<&str>,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-source-preimage-v2\0");
    update_subagent_source_preimage(&mut digest, provider_kind.as_bytes())?;
    update_subagent_source_preimage(&mut digest, source_key.as_bytes())?;
    match capture_incarnation {
        Some(incarnation) => {
            digest.update([1]);
            update_subagent_source_preimage(&mut digest, incarnation.as_bytes())?;
        }
        None => digest.update([0]),
    }
    Ok(digest.finalize().into())
}

fn is_subagent_source_commitment_v2(value: &str) -> bool {
    let Some(value) = value.strip_prefix("source/subagent-hmac-v2/") else {
        return false;
    };
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// V1 checkpoint metadata/traces are immutable historical proof. A matching
/// V1 claim is therefore not silently rewritten or shadowed with another V2
/// source row: recovery must remain on its legacy path until an explicit,
/// future migration can prove all immutable references consistently.
async fn reject_matching_legacy_subagent_claim(
    conn: &DatabaseConnection,
    parent_session_id: &str,
    provider_kind: &str,
    legacy_source_key: &str,
    deadline: Option<Instant>,
) -> Result<()> {
    let legacy = await_subagent_precommit_read_until(deadline, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?",
            [
                parent_session_id.into(),
                provider_kind.into(),
                legacy_source_key.into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION_V1.into(),
            ],
        ))
        .await
        .context("check legacy subagent content proof before V2 materialization")
    })
    .await?;
    if legacy.is_some() {
        bail!(
            "subagent content has a legacy immutable source proof; inspect it with `libra agent doctor` before capturing this source again"
        );
    }
    Ok(())
}

/// Discover Claude's `<project>/<session>/subagents/*.jsonl` sources while
/// holding the directory inode and opening every file via `openat` +
/// `O_NOFOLLOW`. Persistence receives only a SHA-256 digest of the authorized
/// provider-root-relative identity, never its local project slug or filename.
pub(crate) fn discover_claude_subagent_contents(
    cwd: &Path,
    provider_session_id: &str,
    deadline: Option<Instant>,
    byte_budget: u64,
    source_limit: usize,
) -> Result<SubagentDiscovery> {
    // Historical imports may carry legitimate provider identifiers from old
    // Claude versions that do not satisfy the current hex/dash disk naming
    // contract. Such an id is not safe to interpolate into a provider path,
    // so content discovery is unavailable for it while the parent import
    // remains backward-compatible.
    if !claude_session_id_is_safe_path_component(provider_session_id) {
        return Ok(SubagentDiscovery {
            warning: Some(
                "subagent content discovery skipped an unsafe legacy provider session id"
                    .to_string(),
            ),
            ..SubagentDiscovery::default()
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (cwd, deadline, byte_budget, source_limit);
        return Ok(SubagentDiscovery {
            warning: Some(
                "secure subagent content discovery is unavailable on this platform".to_string(),
            ),
            ..SubagentDiscovery::default()
        });
    }
    #[cfg(unix)]
    let result = (|| -> Result<SubagentDiscovery> {
        ensure_before_deadline(deadline)?;
        let Some(project_dir) = claude_session_dir(cwd) else {
            return Ok(SubagentDiscovery::default());
        };
        let subagents_dir = project_dir.join(provider_session_id).join("subagents");
        let adapter = ClaudeCodeObservedAgent::new();
        let Some(directory) = open_provider_directory_for_discovery(&adapter, &subagents_dir)?
        else {
            return Ok(SubagentDiscovery::default());
        };
        let mut names = Vec::new();
        let mut entry_count = 0usize;
        for entry in super::observed_agents::read_dir_pinned_provider_directory(&directory)
            .context("enumerate pinned Claude subagent directory")?
        {
            ensure_before_deadline(deadline)?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_SUBAGENT_DIRECTORY_ENTRIES {
                bail!(
                    "Claude subagent directory exceeds {} entry safety limit",
                    MAX_SUBAGENT_DIRECTORY_ENTRIES
                );
            }
            let entry = entry.context("read pinned Claude subagent directory entry")?;
            let file_type = entry.file_type;
            if file_type.is_symlink() {
                bail!("refusing symlink in Claude subagent directory (fail-closed)");
            }
            let name = entry.file_name;
            if Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                != Some("jsonl")
            {
                continue;
            }
            if !file_type.is_file() {
                bail!("refusing non-regular Claude subagent JSONL source");
            }
            if names.len() >= source_limit {
                bail!("Claude subagent directory exceeds {source_limit} source capture limit");
            }
            names.push(name);
        }
        names.sort();

        let mut total_bytes = 0u64;
        let mut discovered = Vec::with_capacity(names.len());
        for name in names {
            ensure_before_deadline(deadline)?;
            let name_text = name
                .to_str()
                .context("Claude subagent source name is not valid UTF-8")?;
            let mut file =
                open_file_beneath_pinned_provider_directory(&directory, Path::new(name_text))?;
            let length = file
                .metadata()
                .context("inspect securely opened Claude subagent source")?
                .len();
            let effective_budget = byte_budget.min(TRANSCRIPT_READ_HARD_CAP_BYTES);
            if length > effective_budget || total_bytes.saturating_add(length) > effective_budget {
                bail!(
                    "Claude subagent transcript set exceeds {effective_budget} byte input budget"
                );
            }
            let remaining = effective_budget.saturating_sub(total_bytes);
            let bytes = match read_strictly_bounded(&mut file, remaining) {
                StrictBoundedRead::Complete(bytes) => bytes,
                StrictBoundedRead::Oversize { .. } => {
                    bail!(
                        "Claude subagent transcript set grew beyond {effective_budget} byte input budget while reading"
                    );
                }
                StrictBoundedRead::Failed { error, .. } => {
                    return Err(error).context("read securely opened Claude subagent source");
                }
            };
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            // Compute the completeness bit inside the killable discovery
            // helper too; callers need it before import identity finalization.
            let (malformed_lines, partial) = subagent_source_completeness(&bytes, deadline)?;
            let authorized_relative_source = format!(
                "{}/{provider_session_id}/subagents/{name_text}",
                claude_project_slug(cwd)
            );
            // Persist only an opaque digest of the provider-root-relative source.
            // The raw slug/filename remains an in-memory authorization input and
            // cannot leak usernames or customer names into SQLite/traces metadata.
            let source_key = format!(
                "source/sha256/{}",
                hex::encode(Sha256::digest(authorized_relative_source.as_bytes()))
            );
            validate_source_identity("claude_code", &source_key)?;
            discovered.push(DiscoveredSubagentContent {
                provider_kind: "claude_code".to_string(),
                source_key,
                legacy_source_key: None,
                bytes,
                malformed_lines,
                partial,
                // Claude's disk filename is not a provider guarantee that also
                // appears on hook boundaries, so guessing from it is forbidden.
                stable_subagent_id: None,
            });
        }
        Ok(SubagentDiscovery {
            sources: discovered,
            bytes_read: total_bytes,
            warning: None,
            incomplete: false,
        })
    })();
    #[cfg(unix)]
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationOutcome {
    Reserved { fence_token: i64, revision: i64 },
    Unchanged { checkpoint_exists: bool },
    Inflight { lease_expires_at: i64 },
    DurabilityProofStale,
}

struct ReservationAttempt<'a> {
    content_digest: &'a str,
    checkpoint_id: &'a str,
    owner: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DurableCheckpointIdentity {
    traces_commit: String,
    tree_oid: String,
    metadata_blob_oid: String,
}

#[derive(Default)]
struct UnchangedDurabilityProof {
    traces_head: Option<String>,
    checkpoints: HashMap<String, DurableCheckpointIdentity>,
    source_checkpoints: HashMap<(String, String, String), String>,
}

impl UnchangedDurabilityProof {
    fn source_checkpoint(
        &self,
        source: &DiscoveredSubagentContent,
        content_digest: &str,
    ) -> Option<&str> {
        self.source_checkpoints
            .get(&(
                source.provider_kind.clone(),
                source.source_key.clone(),
                content_digest.to_string(),
            ))
            .map(String::as_str)
    }
}

/// Acquire the SQLite writer slot while the monotonic half of a normal
/// child-content invocation is still live. The paired SQLite half is checked
/// later by the final authorization immediately before the non-cancellable
/// COMMIT.
async fn begin_subagent_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?
    .with_context(|| format!("{operation}: acquire subagent database writer"))
}

/// Bound only pure SQLite reads that precede a subagent-content mutation.
///
/// A dispatched DML statement, final authorization, and COMMIT are
/// intentionally excluded: cancelling any of those futures would leave the
/// durable outcome unknowable. Callers retain their transaction and roll it
/// back explicitly when this read-side deadline expires.
async fn await_subagent_precommit_read_until<T>(
    deadline: Option<Instant>,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_before_deadline(deadline)?;
    let value = match deadline {
        Some(deadline) => tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), read)
            .await
            .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?,
        None => read.await,
    }?;
    ensure_before_deadline(deadline)?;
    Ok(value)
}

/// Cleanup may only release a reservation that this invocation already owns.
/// Its paired recovery deadline bounds writer acquisition and the final SQLite
/// authorization. COMMIT remains non-cancellable once that authorization has
/// accepted the release.
async fn begin_subagent_cleanup_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow::Error::from(SubagentCaptureDeadline))?
    .with_context(|| format!("{operation}: acquire subagent cleanup database writer"))
}

/// Make final SQLite authorization the last SQL statement in a standalone
/// child-content transaction, then await COMMIT without cancellation. SQLx
/// may dispatch COMMIT before a cancelled acknowledgement future is dropped,
/// so wrapping COMMIT in `timeout_at` cannot prove an expired capture left no
/// new claim, link, or marker.
async fn commit_subagent_transaction_until(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    if let Some(deadline) = deadline
        && let Err(error) = ensure_before_deadline(Some(deadline.monotonic()))
    {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }
    let authorization = authorize_final_capture_commit(Some(scope), &txn, deadline).await;
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(match error {
            CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                anyhow::Error::from(SubagentCaptureDeadline)
            }
            error @ (CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
            | CaptureFinalCommitAuthorizationError::Database(_)) => anyhow::Error::new(error),
        })
        .with_context(|| format!("{operation}: authorize final subagent database commit"));
    }
    txn.commit().await.with_context(|| operation)
}

/// Reservation probes normally perform no durable write. If they had to clear
/// an expired reservation while proving an unchanged leaf, that narrow change
/// is cleanup of an existing provisional row and must settle within its paired
/// recovery deadline; otherwise roll the read-only transaction back.
async fn finish_subagent_reservation_probe(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    cleanup_deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    if let Some(cleanup_deadline) = cleanup_deadline {
        commit_subagent_transaction_until(txn, scope, Some(cleanup_deadline), operation).await
    } else {
        txn.rollback().await.with_context(|| operation)
    }
}

async fn reserve_source(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    attempt: ReservationAttempt<'_>,
    durability_proof: &UnchangedDurabilityProof,
    deadline: CaptureCommitDeadline,
) -> Result<ReservationOutcome> {
    // This is the first durable child-content sink. Keep the invariant here,
    // rather than relying only on the discovery/capture caller, because test
    // and recovery seams can call reservation directly.
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent content reservation requires a V2 source commitment");
    }
    let ReservationAttempt {
        content_digest,
        checkpoint_id,
        owner,
    } = attempt;
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let now_ms = Utc::now().timestamp_millis();
    let txn = begin_subagent_write_transaction_until(
        conn,
        deadline,
        "begin subagent content reservation",
    )
    .await?;
    if let Err(error) = await_subagent_precommit_read_until(
        Some(deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before reserving subagent content");
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let lease_expires_at = now_ms.saturating_add(SUBAGENT_CONTENT_LEASE_MS);
    let inserted = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                current_revision, current_checkpoint_id, current_digest, state,
                attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, NULL, NULL, 'reserved', ?, ?, ?, ?, 1, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key, content_schema_version)
             DO NOTHING",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                content_digest.into(),
                checkpoint_id.into(),
                owner.into(),
                lease_expires_at.into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("insert initial subagent content reservation")?;
    if inserted.rows_affected() == 1 {
        commit_subagent_transaction_until(
            txn,
            scope,
            Some(deadline),
            "commit initial subagent content reservation",
        )
        .await?;
        return Ok(ReservationOutcome::Reserved {
            fence_token: 1,
            revision: 1,
        });
    }

    let mut cleanup_deadline = None;
    let row = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT revision_cursor, current_revision, current_checkpoint_id, current_digest, state,
                    attempt_digest, lease_expires_at, fence_token
             FROM agent_subagent_content_claim
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read existing subagent content claim")
    })
    .await
    {
        Ok(row) => row,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    }
    .context("subagent content claim disappeared during reservation")?;
    let revision_cursor: i64 = row.try_get_by("revision_cursor")?;
    let current_revision: i64 = row.try_get_by("current_revision")?;
    let current_checkpoint_id: Option<String> = row.try_get_by("current_checkpoint_id")?;
    let current_digest: Option<String> = row.try_get_by("current_digest")?;
    let state: String = row.try_get_by("state")?;
    let attempt_digest: Option<String> = row.try_get_by("attempt_digest")?;
    let current_lease: Option<i64> = row.try_get_by("lease_expires_at")?;
    let fence_token: i64 = row.try_get_by("fence_token")?;
    if state == "reserved" && current_lease.is_some_and(|lease| lease > now_ms) {
        let lease_expires_at = current_lease.unwrap_or(now_ms);
        txn.rollback()
            .await
            .context("roll back in-flight subagent content probe")?;
        tracing::debug!(
            same_digest = attempt_digest.as_deref() == Some(content_digest),
            "subagent content source is owned by another live writer"
        );
        return Ok(ReservationOutcome::Inflight { lease_expires_at });
    }
    if state == "reserved" && current_digest.as_deref() == Some(content_digest) {
        let deadline_for_cleanup = subagent_cleanup_deadline(deadline)?;
        ensure_before_deadline(Some(deadline_for_cleanup.monotonic()))?;
        let cleared = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_subagent_content_claim
                 SET state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                     owner = NULL, lease_expires_at = NULL, fence_token = fence_token + 1,
                     updated_at = ?
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'reserved'
                   AND fence_token = ? AND lease_expires_at <= ?",
                [
                    now_ms.into(),
                    parent_session_id.into(),
                    source.provider_kind.clone().into(),
                    source.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    fence_token.into(),
                    now_ms.into(),
                ],
            ))
            .await
            .context("clear expired subagent content reservation")?;
        if cleared.rows_affected() != 1 {
            txn.rollback()
                .await
                .context("roll back lost expired subagent reservation cleanup")?;
            return Ok(ReservationOutcome::Inflight {
                lease_expires_at: now_ms.saturating_add(25),
            });
        }
        cleanup_deadline = Some(deadline_for_cleanup);
    }
    if current_digest.as_deref() == Some(content_digest) {
        let probe_deadline = cleanup_deadline.unwrap_or(deadline);
        let current_checkpoint_id = current_checkpoint_id.as_deref().context(
            "subagent content claim has a digest but no current checkpoint; run `libra agent doctor`",
        )?;
        let intact =
            match await_subagent_precommit_read_until(Some(probe_deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT c.traces_commit, c.tree_oid, c.metadata_blob_oid
                     FROM agent_checkpoint c
                     JOIN agent_subagent_content_revision r
                       ON r.checkpoint_id = c.checkpoint_id
                      AND r.parent_session_id = ?
                      AND r.provider_kind = ?
                      AND r.source_key = ?
                      AND r.content_schema_version = ?
                      AND r.revision = ?
                      AND r.content_digest = ?
                     JOIN agent_subagent_link l
                       ON l.content_checkpoint_id = c.checkpoint_id
                      AND l.parent_session_id = ?
                     WHERE c.checkpoint_id = ? AND c.session_id = ? AND c.scope = 'subagent'",
                    [
                        parent_session_id.into(),
                        source.provider_kind.clone().into(),
                        source.source_key.clone().into(),
                        SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                        current_revision.into(),
                        content_digest.into(),
                        parent_session_id.into(),
                        current_checkpoint_id.into(),
                        parent_session_id.into(),
                    ],
                ))
                .await
                .context("verify unchanged subagent content relation")
            })
            .await
            {
                Ok(intact) => intact,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            };
        if intact.is_none() {
            bail!(
                "subagent content current leaf is incomplete; run `libra agent doctor` before replaying this source"
            );
        }
        let intact = intact.context("unchanged subagent content relation disappeared")?;
        let traces_commit: String = intact.try_get_by("traces_commit")?;
        let tree_oid: String = intact.try_get_by("tree_oid")?;
        let metadata_blob_oid: String = intact.try_get_by("metadata_blob_oid")?;
        let expected = DurableCheckpointIdentity {
            traces_commit,
            tree_oid,
            metadata_blob_oid,
        };
        let proven_checkpoint = durability_proof.source_checkpoint(source, content_digest);
        if proven_checkpoint != Some(current_checkpoint_id)
            || durability_proof.checkpoints.get(current_checkpoint_id) != Some(&expected)
        {
            finish_subagent_reservation_probe(
                txn,
                scope,
                cleanup_deadline,
                "finish stale subagent durability proof probe",
            )
            .await?;
            return Ok(ReservationOutcome::DurabilityProofStale);
        }
        let current_head =
            match await_subagent_precommit_read_until(Some(probe_deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .context("recheck traces head for unchanged subagent content")
            })
            .await
            {
                Ok(current_head) => current_head,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            }
            .map(|row| row.try_get_by::<Option<String>, _>("commit"))
            .transpose()?
            .flatten();
        if current_head != durability_proof.traces_head {
            finish_subagent_reservation_probe(
                txn,
                scope,
                cleanup_deadline,
                "finish changed traces-head durability probe",
            )
            .await?;
            return Ok(ReservationOutcome::DurabilityProofStale);
        }
        finish_subagent_reservation_probe(
            txn,
            scope,
            cleanup_deadline,
            "finish unchanged subagent content probe",
        )
        .await?;
        return Ok(ReservationOutcome::Unchanged {
            checkpoint_exists: true,
        });
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let updated = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'reserved', attempt_digest = ?, attempt_checkpoint_id = ?,
                 owner = ?, lease_expires_at = ?, fence_token = fence_token + 1,
                 updated_at = ?
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ?
               AND (state = 'idle' OR lease_expires_at <= ?)",
            [
                content_digest.into(),
                checkpoint_id.into(),
                owner.into(),
                lease_expires_at.into(),
                now_ms.into(),
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("take over subagent content reservation")?;
    if updated.rows_affected() != 1 {
        txn.rollback()
            .await
            .context("roll back lost subagent content reservation")?;
        return Ok(ReservationOutcome::Inflight {
            lease_expires_at: now_ms.saturating_add(25),
        });
    }
    let fence_row = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT fence_token FROM agent_subagent_content_claim
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ?",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read subagent content reservation fence")
    })
    .await
    {
        Ok(fence_row) => fence_row,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    }
    .context("subagent content claim disappeared after takeover")?;
    let fence_token: i64 = fence_row.try_get_by("fence_token")?;
    commit_subagent_transaction_until(
        txn,
        scope,
        Some(deadline),
        "commit subagent content reservation",
    )
    .await?;
    Ok(ReservationOutcome::Reserved {
        fence_token,
        revision: revision_cursor.saturating_add(1),
    })
}

struct ReservationRelease<'a> {
    parent_session_id: &'a str,
    source: &'a DiscoveredSubagentContent,
    checkpoint_id: &'a str,
    owner: &'a str,
    fence_token: i64,
}

async fn release_reservation(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    release: ReservationRelease<'_>,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let ReservationRelease {
        parent_session_id,
        source,
        checkpoint_id,
        owner,
        fence_token,
    } = release;
    let cleanup_deadline = subagent_cleanup_deadline(deadline)?;
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent content reservation release requires a V2 source commitment");
    }
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    subagent_content_test_failpoint("before_release_reservation")?;
    let txn = begin_subagent_cleanup_transaction_until(
        conn,
        cleanup_deadline,
        "begin subagent content reservation release",
    )
    .await?;
    if let Err(error) = await_subagent_precommit_read_until(
        Some(cleanup_deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before releasing subagent content reservation",
        );
    }
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    let released = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                 owner = NULL, lease_expires_at = NULL, updated_at = ?
             WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
               AND content_schema_version = ? AND state = 'reserved'
               AND attempt_checkpoint_id = ? AND owner = ? AND fence_token = ?",
            [
                Utc::now().timestamp_millis().into(),
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                checkpoint_id.into(),
                owner.into(),
                fence_token.into(),
            ],
        ))
        .await
        .with_context(|| {
            format!("release failed subagent content reservation for checkpoint {checkpoint_id}")
        })?;
    if released.rows_affected() != 1 {
        bail!(
            "failed to release subagent content reservation for checkpoint {checkpoint_id}: \
             its ownership fence changed; run `libra agent doctor` before retrying"
        );
    }
    // Cleanup has a distinct short recovery grace, but it must still stop
    // before final authorization if that grace itself is exhausted.
    ensure_before_deadline(Some(cleanup_deadline.monotonic()))?;
    commit_subagent_transaction_until(
        txn,
        scope,
        Some(cleanup_deadline),
        "commit subagent content reservation release",
    )
    .await?;
    Ok(())
}

fn preserve_cleanup_error(
    primary: anyhow::Error,
    cleanup_operation: &str,
    cleanup: anyhow::Error,
) -> anyhow::Error {
    primary.context(format!(
        "{cleanup_operation} also failed: {cleanup:#}; run `libra agent doctor` before retrying"
    ))
}

/// Markers are mutable recovery evidence. The traces API validates the scope
/// inside its own registration transaction, avoiding a check-to-write race.
async fn register_traces_write_attempt_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    marker: &TracesInflightMarker,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    // The traces API bounds its preparation and performs the final SQLite
    // authorization itself. Do not wrap this call in another timeout: that
    // could cancel an acknowledgement after its COMMIT was dispatched.
    match crate::internal::ai::traces::register_traces_write_attempt_with_capture_scope_until(
        conn,
        Some(scope),
        marker,
        &[],
        Some(CheckpointScope::Subagent),
        None,
        deadline,
    )
    .await
    .map_err(normalize_subagent_marker_deadline)
    .context("register subagent content traces write attempt")?
    {
        crate::internal::ai::traces::TracesWriteAttemptRegistration::Registered => Ok(()),
        crate::internal::ai::traces::TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration => {
            bail!("subagent content traces writer attempt is already in flight")
        }
        crate::internal::ai::traces::TracesWriteAttemptRegistration::AlreadyCommitted => {
            bail!("subagent content checkpoint is already durably committed")
        }
        crate::internal::ai::traces::TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete => {
            bail!("subagent content marker registration received an invalid terminal receipt acknowledgement")
        }
    }
}

/// A stale owner must also leave a marker alone: the traces API validates the
/// scope inside its cleanup transaction, preserving doctor/GC evidence when
/// the lease has expired.
async fn clear_non_cleanup_traces_inflight_marker_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    checkpoint_id: &str,
    marker_generation: &str,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    ensure_before_deadline(Some(deadline.monotonic()))?;
    // The traces API owns both the final authorization and the unbounded
    // COMMIT acknowledgement for this ordinary (non-cleanup) mutation.
    crate::internal::ai::traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
        conn,
        Some(scope),
        parent_session_id,
        checkpoint_id,
        marker_generation,
        deadline,
    )
    .await
    .map_err(normalize_subagent_marker_deadline)
    .context("clear subagent content in-flight marker")?;
    Ok(())
}

async fn unique_boundary_checkpoint<C: ConnectionTrait>(
    conn: &C,
    parent_session_id: &str,
    stable_subagent_id: Option<&str>,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    let Some(stable_id) = stable_subagent_id else {
        return Ok(None);
    };
    let rows = await_subagent_precommit_read_until(deadline, async {
        conn.query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint
             WHERE session_id = ? AND scope = 'subagent'
               AND (subagent_session_id = ? OR tool_use_id = ?)
             ORDER BY created_at, checkpoint_id LIMIT 2",
            [parent_session_id.into(), stable_id.into(), stable_id.into()],
        ))
        .await
        .context("resolve stable subagent boundary association")
    })
    .await?;
    if rows.len() != 1 {
        return Ok(None);
    }
    rows.into_iter()
        .next()
        .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
        .transpose()
        .context("read unique subagent boundary checkpoint")
}

async fn refresh_current_link(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    parent_session_id: &str,
    source: &DiscoveredSubagentContent,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    if !is_subagent_source_commitment_v2(&source.source_key) {
        bail!("subagent link refresh requires a V2 source commitment");
    }
    let Some(stable_id) = source.stable_subagent_id.as_deref() else {
        return Ok(());
    };
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let txn = begin_subagent_write_transaction_until(conn, deadline, "begin subagent link refresh")
        .await?;
    if let Err(error) = await_subagent_precommit_read_until(
        Some(deadline.monotonic()),
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before refreshing subagent link");
    }
    ensure_before_deadline(Some(deadline.monotonic()))?;
    let current = match await_subagent_precommit_read_until(Some(deadline.monotonic()), async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT current_checkpoint_id FROM agent_subagent_content_claim
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND current_revision > 0",
            [
                parent_session_id.into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("read current subagent checkpoint for link refresh")
    })
    .await
    {
        Ok(current) => current,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let Some(current) = current else {
        txn.rollback()
            .await
            .context("roll back empty subagent link refresh")?;
        return Ok(());
    };
    let checkpoint_id: Option<String> = current.try_get_by("current_checkpoint_id")?;
    let Some(checkpoint_id) = checkpoint_id else {
        txn.rollback()
            .await
            .context("roll back unmaterialized subagent link refresh")?;
        return Ok(());
    };
    let boundary = match unique_boundary_checkpoint(
        &txn,
        parent_session_id,
        Some(stable_id),
        Some(deadline.monotonic()),
    )
    .await
    {
        Ok(boundary) => boundary,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let Some(boundary) = boundary else {
        txn.rollback()
            .await
            .context("roll back unresolved subagent link refresh")?;
        return Ok(());
    };
    ensure_before_deadline(Some(deadline.monotonic()))?;
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE agent_subagent_link
         SET link_state = 'resolved', boundary_checkpoint_id = ?,
             sync_revision = sync_revision + 1, updated_at = ?
         WHERE content_checkpoint_id = ? AND parent_session_id = ?
           AND stable_subagent_id = ? AND link_state = 'unresolved'",
        [
            boundary.into(),
            Utc::now().timestamp_millis().into(),
            checkpoint_id.into(),
            parent_session_id.into(),
            stable_id.into(),
        ],
    ))
    .await
    .context("resolve subagent content link without rewriting checkpoint history")?;
    commit_subagent_transaction_until(txn, scope, Some(deadline), "commit subagent link refresh")
        .await?;
    Ok(())
}

#[derive(Debug)]
struct ContentCommitPlan {
    capture_scope: CaptureScope,
    parent_session_id: String,
    provider_kind: String,
    source_key: String,
    content_digest: String,
    checkpoint_id: String,
    owner: String,
    fence_token: i64,
    revision: i64,
    parent_commit: Option<String>,
    source_channel: String,
    partial: bool,
    stable_subagent_id: Option<String>,
    created_at: i64,
    deadline: CaptureCommitDeadline,
}

impl ContentCommitPlan {
    async fn apply_until(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // The transaction writes the claim, revision, checkpoint metadata,
        // and link. Reject a forged direct plan before any of those durable
        // surfaces can receive a provider locator or legacy SHA proof.
        if !is_subagent_source_commitment_v2(&self.source_key) {
            bail!("subagent content finalization requires a V2 source commitment");
        }
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        await_subagent_precommit_read_until(
            Some(self.deadline.monotonic()),
            self.capture_scope.assert_workspace_fence_live(txn),
        )
        .await
        .context("verify capture workspace lease before final subagent content transaction")?;
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        subagent_content_test_failpoint("before_final_sql")?;
        let writable =
            await_subagent_precommit_read_until(Some(self.deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_session s
                     WHERE s.session_id = ?
                       AND NOT EXISTS (
                         SELECT 1 FROM agent_import_tombstone t
                         WHERE t.agent_kind = s.agent_kind
                           AND t.provider_session_id = s.provider_session_id
                       )",
                    [self.parent_session_id.clone().into()],
                ))
                .await
                .context("verify subagent content tombstone write barrier")
            })
            .await?;
        if writable.is_none() {
            bail!("parent agent session was erased while subagent content was in flight");
        }

        let reservation =
            await_subagent_precommit_read_until(Some(self.deadline.monotonic()), async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT revision_cursor FROM agent_subagent_content_claim
                     WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                       AND content_schema_version = ? AND state = 'reserved'
                       AND attempt_digest = ? AND attempt_checkpoint_id = ?
                       AND owner = ? AND fence_token = ?",
                    [
                        self.parent_session_id.clone().into(),
                        self.provider_kind.clone().into(),
                        self.source_key.clone().into(),
                        SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                        self.content_digest.clone().into(),
                        self.checkpoint_id.clone().into(),
                        self.owner.clone().into(),
                        self.fence_token.into(),
                    ],
                ))
                .await
                .context("verify subagent content reservation fence")
            })
            .await?
            .context("subagent content reservation was lost before final commit")?;
        let revision_cursor: i64 = reservation.try_get_by("revision_cursor")?;
        if self.revision != revision_cursor.saturating_add(1) {
            bail!("subagent content revision changed while writer was in flight");
        }

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, ?, NULL, 'subagent', ?, ?, ?, ?, NULL, ?, ?, ?)",
            [
                self.checkpoint_id.clone().into(),
                self.parent_session_id.clone().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                // Content provenance keeps the stable id in the association
                // table. This catalog column is reserved for boundary
                // evidence; populating it here would let content match itself.
                Option::<String>::None.into(),
                "subagent content".into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("insert subagent content checkpoint catalog row")?;

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_revision (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision, checkpoint_id, content_digest, source_channel, partial, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                self.parent_session_id.clone().into(),
                self.provider_kind.clone().into(),
                self.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                self.revision.into(),
                self.checkpoint_id.clone().into(),
                self.content_digest.clone().into(),
                self.source_channel.clone().into(),
                i64::from(self.partial).into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("append subagent content source revision")?;

        let boundary = unique_boundary_checkpoint(
            txn,
            &self.parent_session_id,
            self.stable_subagent_id.as_deref(),
            Some(self.deadline.monotonic()),
        )
        .await?;
        let link_state = if boundary.is_some() {
            "resolved"
        } else {
            "unresolved"
        };
        let link_timestamp_ms = Utc::now().timestamp_millis();
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
            [
                self.checkpoint_id.clone().into(),
                self.parent_session_id.clone().into(),
                link_state.into(),
                boundary.into(),
                self.stable_subagent_id.clone().into(),
                link_timestamp_ms.into(),
                link_timestamp_ms.into(),
            ],
        ))
        .await
        .context("insert subagent content boundary association")?;

        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        let advanced = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_subagent_content_claim
                 SET revision_cursor = ?, sync_revision = sync_revision + 1,
                     current_revision = ?, current_checkpoint_id = ?, current_digest = ?,
                     state = 'idle', attempt_digest = NULL, attempt_checkpoint_id = NULL,
                     owner = NULL, lease_expires_at = NULL, updated_at = ?
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'reserved'
                   AND attempt_digest = ? AND attempt_checkpoint_id = ?
                   AND owner = ? AND fence_token = ? AND revision_cursor = ?",
                [
                    self.revision.into(),
                    self.revision.into(),
                    self.checkpoint_id.clone().into(),
                    self.content_digest.clone().into(),
                    Utc::now().timestamp_millis().into(),
                    self.parent_session_id.clone().into(),
                    self.provider_kind.clone().into(),
                    self.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    self.content_digest.clone().into(),
                    self.checkpoint_id.clone().into(),
                    self.owner.clone().into(),
                    self.fence_token.into(),
                    revision_cursor.into(),
                ],
            ))
            .await
            .context("advance current subagent content leaf")?;
        if advanced.rows_affected() != 1 {
            bail!("subagent content current leaf lost its fenced final update");
        }
        ensure_before_deadline(Some(self.deadline.monotonic()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl TracesTxnExtra for ContentCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // `HistoryManager` owns final authorization and the following
        // non-cancellable COMMIT. An outer timeout here could cancel that
        // acknowledgement after SQLite already accepted it.
        self.apply_until(txn, ctx).await
    }
}

async fn current_parent_commit(
    conn: &DatabaseConnection,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    await_subagent_precommit_read_until(deadline, async {
        match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
            Ok(commit) => Ok(commit.map(|hash| hash.to_string())),
            Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                if detail.contains("HEAD reference is missing") =>
            {
                Ok(None)
            }
            Err(error) => Err(anyhow!(
                "failed to resolve HEAD for subagent content checkpoint: {error}"
            )),
        }
    })
    .await
}

struct SafeSubagentProjection {
    transcript: RedactedBytes,
    report_value: serde_json::Value,
    partial: bool,
    turn_count: usize,
    content_digest: [u8; 32],
}

/// A serialization sink that refuses to grow the safe child projection beyond
/// the per-source cap. It writes directly into the final buffer so a single
/// pathological JSON turn cannot allocate an uncapped temporary before the
/// caller gets a chance to reject it.
struct BoundedSubagentProjectionWriter<'a> {
    bytes: &'a mut Vec<u8>,
    cap: u64,
}

impl std::io::Write for BoundedSubagentProjectionWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let next_len = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("safe subagent projection length overflow"))?;
        let next_len_u64 = u64::try_from(next_len)
            .map_err(|_| std::io::Error::other("safe subagent projection length overflow"))?;
        if next_len_u64 > self.cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "safe subagent projection exceeds its bounded output cap",
            ));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SafeSubagentProjection {
    #[cfg(test)]
    fn into_parts(self) -> (RedactedBytes, serde_json::Value, bool, usize) {
        (
            self.transcript,
            self.report_value,
            self.partial,
            self.turn_count,
        )
    }

    fn into_parts_with_digest(self) -> (RedactedBytes, serde_json::Value, bool, usize, String) {
        (
            self.transcript,
            self.report_value,
            self.partial,
            self.turn_count,
            hex::encode(self.content_digest),
        )
    }
}

#[cfg(test)]
fn safe_content_projection(
    source: &DiscoveredSubagentContent,
) -> Result<(RedactedBytes, serde_json::Value, bool, usize)> {
    safe_content_projection_bounded(source, u64::MAX).map(SafeSubagentProjection::into_parts)
}

/// Normalize, redact, and serialize child content while retaining an explicit
/// byte ceiling for the projection. Production deadline callers run this
/// function only inside a killable helper; the synchronous wrapper remains
/// for non-deadlined unit fixtures and helper-side execution.
fn safe_content_projection_bounded(
    source: &DiscoveredSubagentContent,
    transcript_cap: u64,
) -> Result<SafeSubagentProjection> {
    let mut turns = normalize_claude_transcript(&source.bytes);
    let report = redact_turns_with_report(&mut turns);
    let partial = source.partial
        || source.malformed_lines > 0
        || turns.is_empty()
        || turns
            .iter()
            .any(|turn| turn.completeness.as_db_str() == "incomplete");
    let mut bytes = Vec::new();
    for turn in &turns {
        let projection = safe_turn_projection(&source.provider_kind, turn);
        let mut writer = BoundedSubagentProjectionWriter {
            bytes: &mut bytes,
            cap: transcript_cap,
        };
        serde_json::to_writer(&mut writer, &projection)
            .context("serialize safe subagent turn projection")?;
        std::io::Write::write_all(&mut writer, b"\n")
            .context("terminate safe subagent turn projection")?;
    }
    let transcript = RedactedBytes::new_unchecked(bytes);
    let content_digest =
        subagent_content_identity_digest_bytes(&transcript, partial, source.malformed_lines);
    Ok(SafeSubagentProjection {
        transcript,
        report_value: serde_json::to_value(report)
            .context("serialize subagent redaction report")?,
        partial,
        turn_count: turns.len(),
        content_digest,
    })
}

/// The request carries only the completeness metadata necessary to preserve
/// the durable content identity. Keeping it in the frame lets the helper hash
/// the final projection, so the deadline-owning parent never scans a large
/// transcript after its child exits.
fn encode_subagent_projection_request_header(
    source: &DiscoveredSubagentContent,
) -> Result<[u8; SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES]> {
    let malformed_lines = u64::try_from(source.malformed_lines)
        .context("subagent projection malformed-line count overflow")?;
    let mut header = [0_u8; SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES];
    header[0] = u8::from(source.partial);
    header[1..].copy_from_slice(&malformed_lines.to_le_bytes());
    Ok(header)
}

fn decode_subagent_projection_request(mut input: Vec<u8>) -> Option<DiscoveredSubagentContent> {
    let input_len = u64::try_from(input.len()).ok()?;
    if input_len > SUBAGENT_PROJECTION_HELPER_INPUT_CAP
        || input.len() < SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES
    {
        return None;
    }
    let partial = match input[0] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let malformed_lines = <[u8; 8]>::try_from(&input[1..SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES])
        .ok()
        .map(u64::from_le_bytes)
        .and_then(|value| usize::try_from(value).ok())?;
    let byte_len = input
        .len()
        .checked_sub(SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES)?;
    if u64::try_from(byte_len).ok()? > TRANSCRIPT_READ_HARD_CAP_BYTES {
        return None;
    }
    // Retain this exact bounded stdin allocation rather than cloning the raw
    // child transcript before normalization/redaction in the helper.
    input.copy_within(SUBAGENT_PROJECTION_REQUEST_HEADER_BYTES.., 0);
    input.truncate(byte_len);
    Some(DiscoveredSubagentContent {
        provider_kind: "claude_code".to_string(),
        source_key: String::new(),
        legacy_source_key: None,
        bytes: input,
        malformed_lines,
        partial,
        stable_subagent_id: None,
    })
}

/// Private helper entry for CPU-heavy child content normalization, typed-field
/// redaction, and allowlist projection. Its input is exactly one bounded raw
/// child transcript on stdin; no provider path, session id, or raw error text
/// crosses the helper protocol.
#[doc(hidden)]
pub fn run_subagent_projection_helper(input: Vec<u8>) -> Result<Vec<u8>> {
    let Some(source) = decode_subagent_projection_request(input) else {
        return Ok(vec![SUBAGENT_PROJECTION_FAILED]);
    };
    let raw_bytes = u64::try_from(source.bytes.len()).ok();
    let response = raw_bytes.and_then(|raw_bytes| {
        safe_content_projection_bounded(&source, subagent_projection_transcript_cap(raw_bytes))
            .ok()
            .and_then(|projection| encode_subagent_projection_frame(projection, raw_bytes))
    });
    Ok(match response {
        Some(response) => response,
        None => vec![SUBAGENT_PROJECTION_FAILED],
    })
}

fn encode_subagent_projection_frame(
    projection: SafeSubagentProjection,
    raw_bytes: u64,
) -> Option<Vec<u8>> {
    let report = serde_json::to_vec(&projection.report_value).ok()?;
    let transcript_len = u64::try_from(projection.transcript.len()).ok()?;
    let report_len = u64::try_from(report.len()).ok()?;
    if transcript_len > subagent_projection_transcript_cap(raw_bytes)
        || report_len > SUBAGENT_PROJECTION_REPORT_CAP
    {
        return None;
    }
    let turn_count = u64::try_from(projection.turn_count).ok()?;
    let report_len_u32 = u32::try_from(report_len).ok()?;
    let total_len = SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
        .checked_add(usize::try_from(transcript_len).ok()?)?
        .checked_add(usize::try_from(report_len).ok()?)?;
    if u64::try_from(total_len).ok()? > subagent_projection_response_cap(raw_bytes) {
        return None;
    }
    let mut frame = Vec::with_capacity(total_len);
    frame.push(SUBAGENT_PROJECTION_COMPLETE);
    frame.push(u8::from(projection.partial));
    frame.extend_from_slice(&turn_count.to_le_bytes());
    frame.extend_from_slice(&transcript_len.to_le_bytes());
    frame.extend_from_slice(&report_len_u32.to_le_bytes());
    frame.extend_from_slice(&projection.content_digest);
    frame.extend_from_slice(projection.transcript.as_ref());
    frame.extend_from_slice(&report);
    Some(frame)
}

fn decode_subagent_projection_frame(
    mut frame: Vec<u8>,
    raw_bytes: u64,
) -> Result<SafeSubagentProjection> {
    if frame == [SUBAGENT_PROJECTION_FAILED] {
        bail!("killable subagent content projection helper reported a safe failure");
    }
    if frame.len() < SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
        || frame.first().copied() != Some(SUBAGENT_PROJECTION_COMPLETE)
    {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let partial = match frame[1] {
        0 => false,
        1 => true,
        _ => bail!("killable subagent content projection helper returned an invalid response"),
    };
    let turn_count = <[u8; 8]>::try_from(&frame[2..10])
        .map(u64::from_le_bytes)
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .context("killable subagent content projection helper returned an invalid response")?;
    let transcript_len = <[u8; 8]>::try_from(&frame[10..18])
        .map(u64::from_le_bytes)
        .ok()
        .context("killable subagent content projection helper returned an invalid response")?;
    let report_len = <[u8; 4]>::try_from(&frame[18..22])
        .map(u32::from_le_bytes)
        .ok()
        .map(u64::from)
        .context("killable subagent content projection helper returned an invalid response")?;
    if transcript_len > subagent_projection_transcript_cap(raw_bytes)
        || report_len > SUBAGENT_PROJECTION_REPORT_CAP
    {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let payload_len = usize::try_from(transcript_len)
        .ok()
        .and_then(|transcript_len| {
            usize::try_from(report_len)
                .ok()
                .and_then(|report_len| transcript_len.checked_add(report_len))
        })
        .context("killable subagent content projection helper returned an invalid response")?;
    let expected_len = SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
        .checked_add(payload_len)
        .context("killable subagent content projection helper returned an invalid response")?;
    let response_cap = subagent_projection_response_cap(raw_bytes);
    if frame.len() != expected_len || u64::try_from(frame.len()).ok() > Some(response_cap) {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let transcript_end =
        SUBAGENT_PROJECTION_FRAME_HEADER_BYTES
            .checked_add(usize::try_from(transcript_len).context(
                "killable subagent content projection helper returned an invalid response",
            )?)
            .context("killable subagent content projection helper returned an invalid response")?;
    let report: RedactionReport = serde_json::from_slice(&frame[transcript_end..])
        .context("decode safe subagent projection redaction report")?;
    if report.matches.len() > MAX_REDACTION_MATCH_SAMPLES {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let content_digest =
        <[u8; 32]>::try_from(&frame[22..SUBAGENT_PROJECTION_FRAME_HEADER_BYTES])
            .context("killable subagent content projection helper returned an invalid response")?;
    // Keep the exact bounded stdout allocation for the redacted projection.
    // Cloning the payload here would briefly add another 1.5x raw buffer to
    // the hook parent and violate ACF-03's two-buffer memory budget.
    frame.copy_within(SUBAGENT_PROJECTION_FRAME_HEADER_BYTES..transcript_end, 0);
    frame.truncate(
        usize::try_from(transcript_len)
            .context("killable subagent content projection helper returned an invalid response")?,
    );
    Ok(SafeSubagentProjection {
        transcript: RedactedBytes::new_unchecked(frame),
        report_value: serde_json::to_value(report)
            .context("serialize safe subagent projection redaction report")?,
        partial,
        turn_count,
        content_digest,
    })
}

/// Run the CPU-heavy projection under the same absolute deadline as child
/// persistence. The parent only copies bounded helper output; it never parses
/// or redacts native child transcript bytes in-process on a live deadline.
#[cfg(unix)]
async fn safe_content_projection_until(
    source: &DiscoveredSubagentContent,
    deadline: Instant,
) -> Result<(RedactedBytes, serde_json::Value, bool, usize, String)> {
    ensure_before_deadline(Some(deadline))?;
    let source_len = u64::try_from(source.bytes.len())
        .context("subagent content projection source length overflow")?;
    if source_len > TRANSCRIPT_READ_HARD_CAP_BYTES {
        bail!("subagent content projection source exceeds its bounded input cap");
    }
    let request_header = encode_subagent_projection_request_header(source)?;
    let Some(program) = projection_helper_program() else {
        #[cfg(test)]
        {
            // Unit tests link the library without the CLI main entrypoint.
            // Production must never take this synchronous path: it would
            // make a deadline-bound capture non-cancellable again.
            let projection = safe_content_projection_bounded(
                source,
                subagent_projection_transcript_cap(source_len),
            )?;
            ensure_before_deadline(Some(deadline))?;
            return Ok(projection.into_parts_with_digest());
        }
        #[cfg(not(test))]
        bail!("killable subagent content projection helper is unavailable in this executable");
    };
    let mut command = tokio::process::Command::new(program);
    command
        .arg(SUBAGENT_PROJECTION_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_private_helper_process_group(&mut command);
    let child = command
        .spawn()
        .context("start killable subagent content projection helper")?;
    let mut child = CancellationSafeChild::new_process_group(child);
    let Some(mut stdin) = child.child_mut().and_then(|child| child.stdin.take()) else {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper has no stdin pipe");
    };
    let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper has no stdout pipe");
    };
    let output_cap = subagent_projection_response_cap(source_len);
    let mut output_task =
        tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
    child.register_abort_on_cancel(&output_task);
    let write_result: Result<()> =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            stdin.write_all(&request_header).await?;
            stdin.write_all(&source.bytes).await?;
            stdin.shutdown().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error).context("send killable subagent projection request"),
            Err(_) => Err(SubagentCaptureDeadline.into()),
        };
    if let Err(error) = write_result {
        child.terminate_and_reap();
        return Err(error);
    }
    // The projection helper consumes its complete request before producing a
    // response. `shutdown` flushes the async writer, but the pipe remains
    // owned until this handle is dropped; otherwise `child.wait()` can deadlock
    // behind a helper still waiting for EOF.
    drop(stdin);
    // Await stdout EOF before reaping the leader. A forked child can retain
    // both request/response pipes; the deadline path must still own the
    // leader PGID when it kills that descendant.
    let output =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut output_task)
            .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                bail!("killable subagent content projection helper returned an invalid response")
            }
            Err(_) => {
                output_task.abort();
                child.terminate_and_reap();
                return Err(SubagentCaptureDeadline.into());
            }
        };
    let output_len = u64::try_from(output.len())
        .context("killable subagent content projection helper output length overflow")?;
    if output_len > output_cap {
        child.terminate_and_reap();
        bail!("killable subagent content projection helper returned an invalid response");
    }
    let status_result = match child.child_mut() {
        Some(child_process) => {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(error)) => {
                    Err(anyhow!(error).context("wait for killable subagent projection helper"))
                }
                Err(_) => Err(SubagentCaptureDeadline.into()),
            }
        }
        None => Err(anyhow!(
            "killable subagent content projection helper was unavailable"
        )),
    };
    let status = match status_result {
        Ok(status) => {
            child.disarm_child_after_wait();
            status
        }
        Err(error) => {
            child.terminate_and_reap();
            return Err(error);
        }
    };
    child.finish();
    if !status.success() {
        bail!("killable subagent content projection helper returned an invalid response");
    }
    ensure_before_deadline(Some(deadline))?;
    let projection = decode_subagent_projection_frame(output, source_len)?;
    ensure_before_deadline(Some(deadline))?;
    Ok(projection.into_parts_with_digest())
}

#[cfg(not(unix))]
async fn safe_content_projection_until(
    _source: &DiscoveredSubagentContent,
    _deadline: Instant,
) -> Result<(RedactedBytes, serde_json::Value, bool, usize, String)> {
    bail!("killable subagent content projection helper is unavailable on this platform")
}

fn subagent_content_identity_digest_bytes(
    transcript: &RedactedBytes,
    partial: bool,
    malformed_lines: usize,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"libra-subagent-content-v1\0");
    digest.update((transcript.as_ref().len() as u64).to_be_bytes());
    digest.update(transcript.as_ref());
    digest.update([u8::from(partial)]);
    digest.update((malformed_lines as u64).to_be_bytes());
    digest.finalize().into()
}

#[cfg(test)]
fn subagent_content_identity_digest(
    transcript: &RedactedBytes,
    partial: bool,
    malformed_lines: usize,
) -> String {
    hex::encode(subagent_content_identity_digest_bytes(
        transcript,
        partial,
        malformed_lines,
    ))
}

struct PreparedSubagentContent<'a> {
    source: &'a DiscoveredSubagentContent,
    transcript: RedactedBytes,
    report_value: serde_json::Value,
    partial: bool,
    turn_count: usize,
    content_digest: String,
}

async fn traces_head<C: ConnectionTrait>(
    conn: &C,
    deadline: Option<Instant>,
) -> Result<Option<String>> {
    await_subagent_precommit_read_until(deadline, async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .context("read refs/libra/traces for subagent durability proof")
    })
    .await?
    .map(|row| row.try_get_by::<Option<String>, _>("commit"))
    .transpose()
    .context("decode refs/libra/traces for subagent durability proof")
    .map(Option::flatten)
}

/// Build one durability proof for every currently unchanged child source.
/// Callers process these sources before any changed source can append a new
/// traces commit, so each per-source reservation only needs to recheck the
/// exact catalog identity and one ref value rather than walking global history.
async fn build_unchanged_durability_proof(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    prepared: &[PreparedSubagentContent<'_>],
    deadline: Option<Instant>,
) -> Result<UnchangedDurabilityProof> {
    let mut candidates = Vec::<(String, DurableCheckpointIdentity)>::new();
    let mut source_checkpoints = HashMap::new();
    for item in prepared {
        let source = item.source;
        let row = await_subagent_precommit_read_until(deadline, async {
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT c.current_checkpoint_id, cp.traces_commit, cp.tree_oid,
                        cp.metadata_blob_oid
                 FROM agent_subagent_content_claim c
                 JOIN agent_subagent_content_revision r
                   ON r.parent_session_id = c.parent_session_id
                  AND r.provider_kind = c.provider_kind
                  AND r.source_key = c.source_key
                  AND r.content_schema_version = c.content_schema_version
                  AND r.revision = c.current_revision
                  AND r.checkpoint_id = c.current_checkpoint_id
                  AND r.content_digest = c.current_digest
                 JOIN agent_checkpoint cp
                   ON cp.checkpoint_id = c.current_checkpoint_id
                  AND cp.session_id = c.parent_session_id
                  AND cp.scope = 'subagent'
                 JOIN agent_subagent_link l
                   ON l.content_checkpoint_id = c.current_checkpoint_id
                  AND l.parent_session_id = c.parent_session_id
                 WHERE c.parent_session_id = ? AND c.provider_kind = ?
                   AND c.source_key = ? AND c.content_schema_version = ?
                   AND c.current_revision > 0 AND c.current_digest = ?",
                [
                    parent_session_id.into(),
                    source.provider_kind.clone().into(),
                    source.source_key.clone().into(),
                    SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                    item.content_digest.clone().into(),
                ],
            ))
            .await
            .context("load unchanged subagent content for batch durability proof")
        })
        .await?;
        let Some(row) = row else {
            continue;
        };
        let checkpoint_id: String = row.try_get_by("current_checkpoint_id")?;
        let identity = DurableCheckpointIdentity {
            traces_commit: row.try_get_by("traces_commit")?,
            tree_oid: row.try_get_by("tree_oid")?,
            metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
        };
        source_checkpoints.insert(
            (
                source.provider_kind.clone(),
                source.source_key.clone(),
                item.content_digest.clone(),
            ),
            checkpoint_id.clone(),
        );
        if !candidates
            .iter()
            .any(|(existing, _)| existing == &checkpoint_id)
        {
            candidates.push((checkpoint_id, identity));
        }
    }
    if candidates.is_empty() {
        return Ok(UnchangedDurabilityProof::default());
    }
    let head_before = traces_head(conn, deadline)
        .await?
        .context(
            "refs/libra/traces is missing while proving unchanged subagent content; run `libra agent doctor` before replaying these sources",
        )?;
    let specs = candidates
        .iter()
        .map(
            |(checkpoint_id, identity)| history::CheckpointDurabilitySpec {
                checkpoint_id,
                traces_commit: &identity.traces_commit,
                tree_oid: &identity.tree_oid,
                metadata_blob_oid: &identity.metadata_blob_oid,
            },
        )
        .collect::<Vec<_>>();
    history::checkpoint_snapshot_durable_oids(conn, storage_root, &specs, deadline)
        .await
        .context(
            "subagent content checkpoint objects or traces reachability are incomplete; run `libra agent doctor` before replaying these sources",
        )?;
    let head_after = traces_head(conn, deadline).await?;
    if head_after.as_deref() != Some(head_before.as_str()) {
        bail!("refs/libra/traces changed during the batch durability proof; retry the capture");
    }
    Ok(UnchangedDurabilityProof {
        traces_head: Some(head_before),
        checkpoints: candidates.into_iter().collect(),
        source_checkpoints,
    })
}

/// Persist every discovered source independently.  A byte-identical current
/// digest is a no-op; changed content advances only that source's revision.
///
/// Kept for in-crate compatibility fixtures; production paths must call the
/// scope-carrying variant below.
#[allow(dead_code)]
pub(crate) async fn capture_discovered_subagent_contents(
    conn: &DatabaseConnection,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<SubagentCaptureSummary> {
    // Compatibility seam for in-process tests and older callers. Real hook
    // and import paths pass the ingress-resolved scope through the explicit
    // API below; this fallback still constructs a typed main-worktree scope
    // instead of accepting an unscoped mutable write.
    let scope = await_subagent_precommit_read_until(
        deadline.map(CaptureCommitDeadline::monotonic),
        CaptureScope::main_for_connection(conn),
    )
    .await?;
    capture_discovered_subagent_contents_with_scope(
        conn,
        &scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
    )
    .await
}

/// Scope-aware child-content capture. The supplied scope must match the
/// parent session's durable ownership tuple exactly; a workspace lease is
/// rechecked at each mutation and again inside the final traces ref-CAS
/// transaction.
pub(crate) async fn capture_discovered_subagent_contents_with_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    storage_root: &Path,
    parent_session_id: &str,
    sources: &[DiscoveredSubagentContent],
    source_channel: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<SubagentCaptureSummary> {
    let mut summary = SubagentCaptureSummary {
        discovered: sources.len(),
        ..SubagentCaptureSummary::default()
    };
    let invocation = SubagentCaptureInvocation {
        conn,
        scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
    };
    if let Err(source) = capture_discovered_subagent_contents_inner(invocation, &mut summary).await
    {
        return Err(SubagentCaptureProgressError { summary, source }.into());
    }
    Ok(summary)
}

/// Shared immutable inputs for one child-content capture. Grouping these
/// keeps the progress-bearing worker below from accumulating independent
/// parameters as capture fencing evolves.
struct SubagentCaptureInvocation<'a> {
    conn: &'a DatabaseConnection,
    scope: &'a CaptureScope,
    storage_root: &'a Path,
    parent_session_id: &'a str,
    sources: &'a [DiscoveredSubagentContent],
    source_channel: &'a str,
    deadline: Option<CaptureCommitDeadline>,
}

async fn capture_discovered_subagent_contents_inner(
    invocation: SubagentCaptureInvocation<'_>,
    summary: &mut SubagentCaptureSummary,
) -> Result<()> {
    let SubagentCaptureInvocation {
        conn,
        scope,
        storage_root,
        parent_session_id,
        sources,
        source_channel,
        deadline,
    } = invocation;
    if !matches!(source_channel, "live" | "import") {
        bail!("invalid subagent content source channel");
    }
    // Programmatic callers predating M5 did not supply a deadline. Bound
    // contention waits anyway so an abandoned live lease can never turn this
    // API into an unbounded hang.
    let effective_deadline = match deadline {
        Some(deadline) => deadline,
        // This compatibility ingress establishes both clock halves together.
        // Lower layers must never reconstruct the SQLite half from a later
        // monotonic Instant.
        None => CaptureCommitDeadline::from_budget(Duration::from_secs(5))
            .context("compute bounded deadline for subagent content projection")?,
    };
    let projection_deadline = effective_deadline.monotonic();
    await_subagent_precommit_read_until(
        Some(projection_deadline),
        scope.assert_workspace_fence_live(conn),
    )
    .await
    .context("verify capture workspace lease before preparing subagent content")?;
    let parent = await_subagent_precommit_read_until(Some(projection_deadline), async {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT agent_kind, provider_session_id, working_dir,
                    json_extract(metadata_json, '$.capture_incarnation') AS capture_incarnation,
                    scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_session WHERE session_id = ?",
            [parent_session_id.into()],
        ))
        .await
        .context("verify subagent content parent session")
    })
    .await?
    .context("subagent content parent session does not exist")?;
    let parent_agent_kind: String = parent.try_get_by("agent_kind")?;
    let parent_provider_session_id: String = parent.try_get_by("provider_session_id")?;
    let parent_working_dir: String = parent.try_get_by("working_dir")?;
    let capture_incarnation: Option<String> = parent.try_get_by("capture_incarnation")?;
    let parent_scope_state: String = parent.try_get_by("scope_state")?;
    let parent_repo_id: Option<String> = parent.try_get_by("repo_id")?;
    let parent_worktree_id: Option<String> = parent.try_get_by("worktree_id")?;
    let parent_workspace_id: Option<String> = parent.try_get_by("workspace_id")?;
    let parent_workspace_fence: Option<i64> = parent.try_get_by("workspace_fence")?;
    let parent_scope = CaptureScope {
        repo_id: parent_repo_id.context(
            "subagent content parent has no repository scope; run `libra worktree doctor` before retrying",
        )?,
        worktree_id: parent_worktree_id.context(
            "subagent content parent has no worktree scope; run `libra worktree doctor` before retrying",
        )?,
        workspace_id: parent_workspace_id,
        workspace_fence: parent_workspace_fence,
    };
    if parent_scope_state != "scoped" || parent_scope != *scope {
        bail!(
            "subagent content parent belongs to a legacy or different capture scope; \
             refusing to mutate it — inspect it with `libra worktree doctor` before retrying"
        );
    }
    await_subagent_precommit_read_until(
        Some(projection_deadline),
        scope.assert_provider_session_compatible(conn, &parent_provider_session_id),
    )
    .await
    .context("verify subagent content parent capture scope")?;
    let parent_working_dir = PathBuf::from(parent_working_dir)
        .canonicalize()
        .context("canonicalize scoped subagent content parent worktree")?;
    let mut incarnation_sources = sources.to_vec();
    for source in &mut incarnation_sources {
        ensure_before_deadline(Some(projection_deadline))?;
        validate_source_identity(&source.provider_kind, &source.source_key)?;
        let legacy_source_key =
            source_key_for_incarnation(&source.source_key, capture_incarnation.as_deref())?;
        reject_matching_legacy_subagent_claim(
            conn,
            parent_session_id,
            &source.provider_kind,
            &legacy_source_key,
            Some(projection_deadline),
        )
        .await?;
        ensure_before_deadline(Some(projection_deadline))?;
        let preimage = subagent_source_preimage(
            &source.provider_kind,
            &source.source_key,
            capture_incarnation.as_deref(),
        )?;
        let commitment = derive_capture_source_commitment_in_scope_until(
            conn,
            scope,
            storage_root,
            &parent_working_dir,
            CaptureSourceCommitmentDomain::SubagentSourceV2,
            &preimage,
            projection_deadline,
        )
        .await
        .context("derive scoped subagent source commitment")?;
        if !is_subagent_source_commitment_v2(&commitment) {
            bail!("derive scoped subagent source commitment returned an invalid proof");
        }
        source.legacy_source_key = Some(legacy_source_key);
        source.source_key = commitment;
    }
    summary.discovered = incarnation_sources.len();
    let mut prepared = Vec::with_capacity(incarnation_sources.len());
    for source in &incarnation_sources {
        ensure_before_deadline(Some(projection_deadline))?;
        if !is_subagent_source_commitment_v2(&source.source_key) {
            bail!("subagent source commitment was not prepared for durable capture");
        }
        if source.provider_kind != parent_agent_kind {
            bail!(
                "subagent content provider does not match its parent capture session (fail-closed)"
            );
        }
        let (transcript, report_value, partial, turn_count, content_digest) =
            safe_content_projection_until(source, projection_deadline).await?;
        prepared.push(PreparedSubagentContent {
            source,
            transcript,
            report_value,
            partial,
            turn_count,
            content_digest,
        });
    }
    let mut durability_proof = build_unchanged_durability_proof(
        conn,
        storage_root,
        parent_session_id,
        &prepared,
        Some(projection_deadline),
    )
    .await?;
    // Proven unchanged leaves must be checked before this invocation appends
    // any changed/new child commit, otherwise its own writes would invalidate
    // the exact-head proof and force another global traversal.
    prepared.sort_by_key(|item| {
        durability_proof
            .source_checkpoint(item.source, &item.content_digest)
            .is_none()
    });
    for index in 0..prepared.len() {
        let item = &prepared[index];
        let source = item.source;
        let transcript = &item.transcript;
        let report_value = &item.report_value;
        let partial = item.partial;
        let turn_count = item.turn_count;
        let content_digest = &item.content_digest;
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let owner = format!("subagent:{}:{}", std::process::id(), uuid::Uuid::new_v4());
        let now_ms = Utc::now().timestamp_millis();
        let mut durability_proof_refreshes = 0_usize;
        let reservation = loop {
            ensure_before_deadline(Some(projection_deadline)).context(
                "another writer still owns the subagent content source; retry the capture",
            )?;
            let outcome = reserve_source(
                conn,
                scope,
                parent_session_id,
                source,
                ReservationAttempt {
                    content_digest,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                },
                &durability_proof,
                effective_deadline,
            )
            .await?;
            match outcome {
                ReservationOutcome::Inflight { lease_expires_at } => {
                    let now = Utc::now().timestamp_millis();
                    let until_lease_ms = lease_expires_at.saturating_sub(now).max(1);
                    let wait_ms = u64::try_from(until_lease_ms).unwrap_or(25).min(25);
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                }
                ReservationOutcome::DurabilityProofStale => {
                    durability_proof_refreshes = durability_proof_refreshes.saturating_add(1);
                    if durability_proof_refreshes > 2 {
                        bail!(
                            "subagent content changed repeatedly while proving durability; retry the capture"
                        );
                    }
                    // A concurrent writer may have published one or more
                    // identical children after the initial batch snapshot.
                    // Rebuild one proof for this child and every remaining
                    // child rather than falling back to per-child traversals.
                    durability_proof = build_unchanged_durability_proof(
                        conn,
                        storage_root,
                        parent_session_id,
                        &prepared[index..],
                        Some(projection_deadline),
                    )
                    .await?;
                }
                settled => break settled,
            }
        };
        let (fence_token, revision) = match reservation {
            ReservationOutcome::Reserved {
                fence_token,
                revision,
            } => (fence_token, revision),
            ReservationOutcome::Unchanged { checkpoint_exists } => {
                if checkpoint_exists {
                    refresh_current_link(
                        conn,
                        scope,
                        parent_session_id,
                        source,
                        effective_deadline,
                    )
                    .await?;
                }
                summary.skipped_unchanged += 1;
                continue;
            }
            ReservationOutcome::Inflight { .. } => {
                bail!("subagent content reservation loop returned an unsettled outcome")
            }
            ReservationOutcome::DurabilityProofStale => {
                bail!("subagent content reservation loop returned a stale durability proof")
            }
        };

        if let Err(error) = subagent_content_test_failpoint("after_reservation") {
            let cleanup = release_reservation(
                conn,
                scope,
                ReservationRelease {
                    parent_session_id,
                    source,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                    fence_token,
                },
                effective_deadline,
            )
            .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    preserve_cleanup_error(error, "release subagent content reservation", cleanup)
                }
            });
        }

        let marker = TracesInflightMarker::new(parent_session_id, &checkpoint_id, now_ms);
        let marker_generation = marker
            .generation
            .as_deref()
            .context("new subagent content marker has no writer generation")?;
        if let Err(error) =
            register_traces_write_attempt_with_scope(conn, scope, &marker, effective_deadline).await
        {
            let cleanup = release_reservation(
                conn,
                scope,
                ReservationRelease {
                    parent_session_id,
                    source,
                    checkpoint_id: &checkpoint_id,
                    owner: &owner,
                    fence_token,
                },
                effective_deadline,
            )
            .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    preserve_cleanup_error(error, "release subagent content reservation", cleanup)
                }
            });
        }

        let written: Result<_> = async {
            subagent_content_test_failpoint("after_marker")?;
            let created_at = Utc::now().timestamp();
            let parent_commit = current_parent_commit(conn, Some(projection_deadline)).await?;
            let metadata_value = serde_json::json!({
                "schema_version": history::CHECKPOINT_METADATA_SCHEMA_VERSION,
                "checkpoint_id": checkpoint_id,
                "session_id": parent_session_id,
                "agent_kind": source.provider_kind,
                "scope": "subagent",
                "parent_checkpoint_id": null,
                "subagent_session_id": source.stable_subagent_id,
                "tool_use_id": null,
                "description": "subagent content",
                "created_at": created_at,
                "subagent": {
                    "provenance": "content",
                    "source_key": source.source_key,
                    "content_schema_version": SUBAGENT_CONTENT_SCHEMA_VERSION,
                    "revision": revision,
                    "source_channel": source_channel,
                    "partial": partial,
                    "malformed_lines": source.malformed_lines,
                    "turn_count": turn_count,
                    "link_state": if source.stable_subagent_id.is_some() {
                        "pending_unique_match"
                    } else {
                        "unresolved"
                    },
                },
            });
            let metadata_bytes = serde_json::to_vec_pretty(&metadata_value)
                .context("serialize subagent content metadata")?;
            let (metadata, _) = Redactor::new_default().redact(&metadata_bytes);
            let report_bytes = serde_json::to_vec_pretty(&report_value)
                .context("serialize subagent content redaction report")?;
            let (report, _) = Redactor::new_default().redact(&report_bytes);
            let empty_events = RedactedBytes::new_unchecked(Vec::new());

            let objects_dir = storage_root.join("objects");
            std::fs::create_dir_all(&objects_dir)
                .context("create objects directory for subagent content")?;
            let manager = HistoryManager::new_with_ref(
                Arc::new(ClientStorage::init(objects_dir)),
                storage_root.to_path_buf(),
                Arc::new(conn.clone()),
                crate::internal::branch::TRACES_BRANCH,
            );
            let plan = ContentCommitPlan {
                capture_scope: scope.clone(),
                parent_session_id: parent_session_id.to_string(),
                provider_kind: source.provider_kind.clone(),
                source_key: source.source_key.clone(),
                content_digest: content_digest.clone(),
                checkpoint_id: checkpoint_id.clone(),
                owner: owner.clone(),
                fence_token,
                revision,
                parent_commit: parent_commit.clone(),
                source_channel: source_channel.to_string(),
                partial,
                stable_subagent_id: source.stable_subagent_id.clone(),
                created_at,
                deadline: effective_deadline,
            };
            let append_deadline =
                final_subagent_append_deadline(deadline, Some(effective_deadline));
            observe_subagent_final_append_deadline(append_deadline);
            manager
                .append_checkpoint_commit(CheckpointCommitParams {
                    checkpoint_id: &checkpoint_id,
                    session_id: parent_session_id,
                    marker_generation,
                    capture_scope: Some(scope),
                    agent_kind: &source.provider_kind,
                    parent_commit: parent_commit.as_deref(),
                    scope: CheckpointScope::Subagent,
                    tool_use_id: None,
                    metadata_json: &metadata,
                    transcript_redacted: transcript,
                    lifecycle_events_jsonl: &empty_events,
                    redaction_report_json: &report,
                    txn_extra: Some(&plan),
                    // The compatibility wrapper synthesizes a short bounded
                    // deadline when legacy in-process callers pass `None`.
                    // Carry that same bound through the final ref/catalog
                    // transaction rather than letting only the earlier
                    // reservation/projection phases be deadline-aware.
                    deadline: append_deadline,
                })
                .await
                .context("append subagent content checkpoint")
        }
        .await;
        let written = match written {
            Ok(written) => written,
            Err(error) => {
                let mut error = error;
                if let Err(cleanup) = release_reservation(
                    conn,
                    scope,
                    ReservationRelease {
                        parent_session_id,
                        source,
                        checkpoint_id: &checkpoint_id,
                        owner: &owner,
                        fence_token,
                    },
                    effective_deadline,
                )
                .await
                {
                    error = preserve_cleanup_error(
                        error,
                        "release subagent content reservation",
                        cleanup,
                    );
                }
                if let Err(cleanup) = clear_non_cleanup_traces_inflight_marker_with_scope(
                    conn,
                    scope,
                    parent_session_id,
                    &checkpoint_id,
                    marker_generation,
                    effective_deadline,
                )
                .await
                {
                    error = preserve_cleanup_error(
                        error,
                        "clear failed subagent content in-flight marker",
                        cleanup,
                    );
                }
                return Err(error);
            }
        };
        if clear_non_cleanup_traces_inflight_marker_with_scope(
            conn,
            scope,
            parent_session_id,
            &checkpoint_id,
            &written.marker_generation,
            effective_deadline,
        )
        .await
        .is_err()
        {
            tracing::warn!(
                cleanup_reason = "clear_committed_subagent_marker_failed",
                "failed to clear committed subagent content in-flight marker"
            );
        }
        summary.checkpoints_written += 1;
        if partial {
            summary.partial_sources += 1;
            tracing::warn!(
                checkpoint_id = %checkpoint_id,
                malformed_lines = source.malformed_lines,
                "subagent content captured partially; malformed lines were skipped"
            );
        }
        subagent_content_test_failpoint("after_first_commit")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use sea_orm::DatabaseConnection;
    use serial_test::serial;

    use super::*;
    use crate::{
        internal::{
            ai::observed_agents::{ObservedAgent, SubagentAwareExtractor},
            config::ConfigKv,
        },
        utils::client_storage::{ObjectIndexTestFaults, install_test_object_index_faults},
    };

    #[cfg(unix)]
    fn write_stalled_helper(path: &Path, pid_file: &Path) {
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled helper");
        let mut permissions = std::fs::metadata(path)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("mark helper executable");
    }

    #[cfg(unix)]
    fn write_pipe_holding_descendant_helper(path: &Path, descendant_file: &Path) {
        let descendant_path = descendant_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s\\n' \"$!\" > '{descendant_path}'\nexit 0\n"
            ),
        )
        .expect("write forking helper");
        let mut permissions = std::fs::metadata(path)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("mark helper executable");
    }

    #[cfg(unix)]
    async fn wait_for_stalled_helper_pid(pid_file: &Path) -> Option<libc::pid_t> {
        // A process-isolated test runner can delay the shell fixture's first
        // scheduling slice. This is readiness synchronization; the deadline
        // assertion begins only after the child is observable.
        for _ in 0..500 {
            if let Ok(value) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = value.trim().parse::<libc::pid_t>()
            {
                return Some(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    #[cfg(unix)]
    async fn wait_for_stalled_helper_reap(pid: libc::pid_t) -> bool {
        for _ in 0..200 {
            // SAFETY: signal zero probes the exact test child PID and does
            // not deliver a signal.
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[test]
    fn unregistered_host_disables_unbounded_discovery() {
        assert!(
            helper_program().is_none(),
            "an embedded test host must not infer a private helper executable"
        );
        let discovery = helper_unavailable_discovery();
        assert!(discovery.sources.is_empty());
        assert_eq!(discovery.bytes_read, 0);
        assert!(
            discovery
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("killable"))
        );
    }

    fn discovery_wire_source_key() -> String {
        format!("source/sha256/{}", "a".repeat(64))
    }

    fn discovery_wire_fixture(raw: &[u8]) -> SubagentDiscovery {
        SubagentDiscovery {
            sources: vec![DiscoveredSubagentContent::fixture(
                "claude_code",
                &discovery_wire_source_key(),
                raw,
                None,
            )],
            bytes_read: u64::try_from(raw.len()).expect("fixture byte count"),
            warning: None,
            incomplete: false,
        }
    }

    fn discovery_frame_header_end(frame: &[u8]) -> usize {
        assert!(frame.len() >= SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES);
        let header_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES + header_len
    }

    fn decode_discovery_wire_for_test(frame: Vec<u8>) -> Result<SubagentDiscovery> {
        decode_subagent_discovery_helper_frame(
            frame,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
            Instant::now() + Duration::from_secs(2),
        )
    }

    #[test]
    fn discovery_helper_frame_keeps_raw_segments_out_of_metadata_and_within_acf03_budget() {
        let raw = b"provider-native-secret-that-must-not-enter-json-metadata";
        let frame = encode_subagent_discovery_helper_success(discovery_wire_fixture(raw))
            .expect("encode direct-raw discovery frame");
        let header_end = discovery_frame_header_end(&frame);
        assert!(
            header_end
                <= SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES
                    + usize::try_from(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP)
                        .expect("header cap fits usize"),
            "metadata header must remain separately bounded"
        );
        assert!(
            !frame[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end]
                .windows(raw.len())
                .any(|window| window == raw),
            "provider-native bytes must not be JSON/base64 encoded into the helper metadata"
        );
        assert_eq!(
            &frame[header_end..],
            raw,
            "the payload tail must carry direct raw source bytes"
        );
        let decoded = decode_discovery_wire_for_test(frame).expect("decode direct-raw frame");
        assert_eq!(decoded.sources.len(), 1);
        assert_eq!(decoded.sources[0].bytes, raw);

        // Parent peak: one preallocated pipe frame (including the one-byte
        // over-cap sentinel), the completed source Vecs, and the owned small
        // metadata header parsed by serde. At the 16 MiB source cap this is
        // 32 MiB + 128 KiB + 1, safely below ACF-03's 2.5x (40 MiB) limit.
        let parent_peak = SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP
            .saturating_add(1)
            .saturating_add(TRANSCRIPT_READ_HARD_CAP_BYTES)
            .saturating_add(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP);
        assert!(
            parent_peak <= TRANSCRIPT_READ_HARD_CAP_BYTES.saturating_mul(5) / 2,
            "direct-raw discovery framing must stay within ACF-03's 2.5x working-set budget"
        );
    }

    #[test]
    fn discovery_helper_frame_rejects_invalid_headers_and_raw_layouts() {
        let raw = b"one bounded child transcript";
        let frame = encode_subagent_discovery_helper_success(discovery_wire_fixture(raw))
            .expect("encode discovery frame");
        let header_end = discovery_frame_header_end(&frame);

        let truncated_header = frame[..header_end - 1].to_vec();
        assert!(
            decode_discovery_wire_for_test(truncated_header).is_err(),
            "a truncated metadata header must fail closed"
        );

        let mut oversized_header = vec![0_u8; SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES];
        let oversized = u32::try_from(SUBAGENT_DISCOVERY_HELPER_HEADER_CAP + 1)
            .expect("test header cap fits u32");
        oversized_header.copy_from_slice(&oversized.to_le_bytes());
        assert!(
            decode_discovery_wire_for_test(oversized_header).is_err(),
            "a metadata header over 64 KiB must fail closed"
        );

        let truncated_raw = frame[..frame.len() - 1].to_vec();
        assert!(
            decode_discovery_wire_for_test(truncated_raw).is_err(),
            "a truncated raw segment must fail closed"
        );

        let mut extra_raw = frame.clone();
        extra_raw.push(b'!');
        assert!(
            decode_discovery_wire_for_test(extra_raw).is_err(),
            "an extra raw tail must fail closed"
        );

        let response: SubagentDiscoveryHelperResponse = serde_json::from_slice(
            &frame[SUBAGENT_DISCOVERY_HELPER_FRAME_HEADER_BYTES..header_end],
        )
        .expect("decode fixture metadata header");
        let mut mismatched_response = response;
        let SubagentDiscoveryHelperResponse::Ok { sources, .. } = &mut mismatched_response else {
            panic!("fixture must encode an OK response");
        };
        sources[0].byte_len = sources[0].byte_len.saturating_add(1);
        let mut mismatched_frame = encode_subagent_discovery_helper_header(&mismatched_response)
            .expect("encode mismatched metadata header");
        mismatched_frame.extend_from_slice(&frame[header_end..]);
        assert!(
            decode_discovery_wire_for_test(mismatched_frame).is_err(),
            "metadata byte lengths must exactly describe the raw tail"
        );
    }

    #[test]
    fn discovery_helper_error_frame_has_no_raw_tail_or_provider_text() {
        let marker = "provider-controlled-path-or-error-text";
        let frame = encode_subagent_discovery_helper_error(false)
            .expect("encode safe discovery helper failure frame");
        assert_eq!(
            discovery_frame_header_end(&frame),
            frame.len(),
            "a safe helper error must have no raw source tail"
        );
        assert!(
            !String::from_utf8_lossy(&frame).contains(marker),
            "a safe helper error frame must not contain provider-controlled text"
        );
        let error = decode_discovery_wire_for_test(frame)
            .expect_err("safe helper failure must not be accepted as discovery");
        assert!(
            error.to_string().contains("helper reported a safe failure"),
            "unexpected safe helper error: {error:#}"
        );
        assert!(
            !error.to_string().contains(marker),
            "parent error must not echo provider-controlled helper text: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_kills_a_stalled_test_helper_at_its_deadline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("stalled-subagent-discovery-helper");
        let pid_file = directory.path().join("stalled-subagent-discovery.pid");
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let started = Instant::now();
        let result = with_subagent_discovery_helper_program(helper, async {
            discover_claude_subagent_contents_bounded(
                directory.path(),
                "bounded-deadline-session",
                // Give the shell fixture enough startup headroom under a
                // parallel test build; the 30-second child still proves the
                // parent enforces its own bounded deadline and reaps it.
                Instant::now() + Duration::from_secs(2),
                1024,
                1,
            )
            .await
        })
        .await;

        assert!(
            result.is_err(),
            "a stalled helper must be killed instead of blocking child discovery"
        );
        assert!(
            format!("{:#}", result.expect_err("stalled helper must time out")).contains("deadline"),
            "the timeout must retain its typed deadline classification"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a killed helper must not outlive the bounded parent deadline"
        );

        let pid = async {
            for _ in 0..100 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("stalled helper must publish its PID before the deadline");
        let reaped = async {
            for _ in 0..50 {
                // SAFETY: signal zero probes the exact test child PID and
                // does not deliver a signal.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "deadline-killed discovery helper was not reaped");
    }

    /// The runtime's outer hook deadline cancels the discovery future before
    /// its own helper deadline. That cancellation must still kill and reap
    /// the direct helper rather than relying on Tokio's best-effort
    /// `kill_on_drop` behavior.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_outer_deadline_cancels_and_reaps_helper() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("outer-timeout-subagent-discovery-helper");
        let pid_file = directory
            .path()
            .join("outer-timeout-subagent-discovery.pid");
        write_stalled_helper(&helper, &pid_file);

        let (result, pid) = {
            let capture = with_subagent_discovery_helper_program(
                helper,
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "outer-timeout-discovery-session",
                    Instant::now() + Duration::from_secs(20),
                    1024,
                    1,
                ),
            );
            tokio::pin!(capture);
            let pid = tokio::select! {
                _ = &mut capture => panic!("stalled discovery helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&pid_file) => {
                    pid.expect("stalled discovery helper must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, pid)
        };
        assert!(
            result.is_err(),
            "the outer host deadline must cancel the still-live discovery helper"
        );
        assert!(
            wait_for_stalled_helper_reap(pid).await,
            "outer-cancelled discovery helper was not reaped"
        );
    }

    /// The discovery leader can exit after forking a child that retains both
    /// inherited pipes. The outer deadline must still kill the group while
    /// the parent intentionally waits for stdout EOF before reaping leader.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_outer_cancellation_kills_pipe_holding_descendant() {
        let directory = tempfile::tempdir().expect("create discovery tempdir");
        let helper = directory.path().join("forking-subagent-discovery-helper");
        let descendant_file = directory.path().join("discovery-descendant.pid");
        write_pipe_holding_descendant_helper(&helper, &descendant_file);

        let (result, descendant) = {
            let capture = with_subagent_discovery_helper_program(
                helper,
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "outer-timeout-discovery-session",
                    Instant::now() + Duration::from_secs(20),
                    1024,
                    1,
                ),
            );
            tokio::pin!(capture);
            let descendant = tokio::select! {
                _ = &mut capture => panic!("pipe-holding discovery helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&descendant_file) => {
                    pid.expect("pipe-holding discovery descendant must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, descendant)
        };
        assert!(
            result.is_err(),
            "outer deadline must cancel discovery with a pipe-holding descendant"
        );
        assert!(
            wait_for_stalled_helper_reap(descendant).await,
            "outer cancellation left a discovery pipe descendant alive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_rejects_stdout_beyond_its_read_cap() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("over-cap-subagent-discovery-helper");
        std::fs::write(
            &helper,
            b"#!/bin/sh\nexec /bin/dd if=/dev/zero bs=1024 count=2 2>/dev/null\n",
        )
        .expect("write over-cap discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let started = Instant::now();
        let result = with_subagent_discovery_helper_program(helper, async {
            with_subagent_discovery_helper_output_cap(1024, async {
                discover_claude_subagent_contents_bounded(
                    directory.path(),
                    "bounded-over-cap-session",
                    Instant::now() + Duration::from_secs(2),
                    1024,
                    1,
                )
                .await
            })
            .await
        })
        .await;

        let error = result.expect_err("over-cap helper output must be rejected");
        assert!(
            error
                .to_string()
                .contains("helper returned an invalid response"),
            "unexpected over-cap helper error: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded stdout drain must not wait for all helper output"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_discovery_helper_failure_is_content_free() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("safe-failure-subagent-discovery-helper");
        let response = directory
            .path()
            .join("safe-failure-subagent-discovery-frame");
        std::fs::write(
            &response,
            encode_subagent_discovery_helper_error(false)
                .expect("encode safe discovery helper failure frame"),
        )
        .expect("write safe failure discovery frame");
        let response_path = response.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nexec /bin/cat '{response_path}'\n"),
        )
        .expect("write safe-failure discovery helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let result = with_subagent_discovery_helper_program(helper, async {
            discover_claude_subagent_contents_bounded(
                directory.path(),
                "bounded-safe-failure-session",
                // This test exercises a safe error frame, not timeout
                // behavior. Leave scheduling headroom for a saturated full
                // suite so it does not turn into a spurious deadline result.
                Instant::now() + Duration::from_secs(5),
                1024,
                1,
            )
            .await
        })
        .await;
        let error = result.expect_err("helper error response must not be accepted");
        assert!(
            error.to_string().contains("helper reported a safe failure"),
            "unexpected helper failure: {error:#}"
        );
        assert!(
            !error.to_string().contains("bounded-safe-failure-session"),
            "helper failure must not echo provider-controlled values: {error:#}"
        );
    }

    #[test]
    fn projection_helper_returns_only_redacted_projection_bytes() {
        let secret = "sk-abcdefghijklmnopqrstuvwx123456";
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/redacted.jsonl",
            &child_transcript(secret),
            None,
        );
        let mut request = encode_subagent_projection_request_header(&source)
            .expect("encode projection request")
            .to_vec();
        request.extend_from_slice(&source.bytes);
        let frame =
            run_subagent_projection_helper(request).expect("run in-process projection helper");
        let projection = decode_subagent_projection_frame(
            frame,
            u64::try_from(source.bytes.len()).expect("fixture source length"),
        )
        .expect("decode safe projection helper frame");
        let projected = String::from_utf8_lossy(projection.transcript.as_ref());
        assert!(
            !projected.contains(secret),
            "raw child secret crossed the projection helper boundary: {projected}"
        );
        assert!(
            projection.turn_count > 0,
            "fixture must produce typed turns"
        );
        assert_eq!(
            hex::encode(projection.content_digest),
            subagent_content_identity_digest(
                &projection.transcript,
                projection.partial,
                source.malformed_lines,
            ),
            "the helper must bind the durable identity to its safe projection"
        );
    }

    #[test]
    fn short_valid_projection_uses_the_fixed_bounded_schema_floor() {
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/short.jsonl",
            &child_transcript("short typed child turn"),
            None,
        );
        let raw_bytes = u64::try_from(source.bytes.len()).expect("fixture source length");
        let transcript_cap = subagent_projection_transcript_cap(raw_bytes);
        assert_eq!(
            transcript_cap, SUBAGENT_PROJECTION_MIN_TRANSCRIPT_CAP,
            "short JSONL sources must retain a fixed, bounded schema allowance"
        );
        let projection = safe_content_projection_bounded(&source, transcript_cap)
            .expect("short valid transcript must fit its bounded projection floor");
        assert!(
            u64::try_from(projection.transcript.len()).expect("projection length")
                <= transcript_cap,
            "projection must remain within the fixed short-source allowance"
        );
        assert!(
            subagent_projection_response_cap(raw_bytes) <= SUBAGENT_PROJECTION_HELPER_OUTPUT_CAP,
            "the fixed short-source allowance must remain within the helper's global response cap"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_deadline_kills_and_reaps_stalled_projection() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("stalled-subagent-projection-helper");
        let pid_file = directory.path().join("stalled-subagent-projection.pid");
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled projection helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/stalled.jsonl",
            &child_transcript("projection fixture"),
            None,
        );

        let started = Instant::now();
        let result = with_subagent_projection_helper_program(helper, async {
            safe_content_projection_until(&source, Instant::now() + Duration::from_secs(2)).await
        })
        .await;
        let error = result.expect_err("stalled projection helper must hit the deadline");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "stalled projection must preserve the capture deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "projection helper outlived its absolute deadline"
        );

        let pid = async {
            for _ in 0..50 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("stalled projection helper must publish its PID");
        let reaped = async {
            for _ in 0..50 {
                // SAFETY: signal zero probes the exact test child PID and
                // never delivers a signal.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "deadline-killed projection helper was not reaped");
    }

    /// A host capture deadline can cancel projection before this helper's own
    /// deadline. The cancellation guard must kill and reap it on that outer
    /// future-drop path too.
    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_outer_deadline_cancels_and_reaps_helper() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("outer-timeout-subagent-projection-helper");
        let pid_file = directory
            .path()
            .join("outer-timeout-subagent-projection.pid");
        write_stalled_helper(&helper, &pid_file);
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/outer-timeout.jsonl",
            &child_transcript("projection outer timeout fixture"),
            None,
        );

        let (result, pid) = {
            let capture = with_subagent_projection_helper_program(
                helper,
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(20)),
            );
            tokio::pin!(capture);
            let pid = tokio::select! {
                _ = &mut capture => panic!("stalled projection helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&pid_file) => {
                    pid.expect("stalled projection helper must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, pid)
        };
        assert!(
            result.is_err(),
            "the outer host deadline must cancel the still-live projection helper"
        );
        assert!(
            wait_for_stalled_helper_reap(pid).await,
            "outer-cancelled projection helper was not reaped"
        );
    }

    /// Projection receives raw child transcript bytes over stdin. A forked
    /// helper descendant retaining that fd and stdout must be group-killed on
    /// outer cancellation even after the direct leader has exited.
    #[cfg(unix)]
    #[tokio::test]
    async fn projection_outer_cancellation_kills_pipe_holding_descendant() {
        let directory = tempfile::tempdir().expect("create projection tempdir");
        let helper = directory.path().join("forking-subagent-projection-helper");
        let descendant_file = directory.path().join("projection-descendant.pid");
        write_pipe_holding_descendant_helper(&helper, &descendant_file);
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/outer-timeout.jsonl",
            &child_transcript("projection descendant fixture"),
            None,
        );

        let (result, descendant) = {
            let capture = with_subagent_projection_helper_program(
                helper,
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(20)),
            );
            tokio::pin!(capture);
            let descendant = tokio::select! {
                _ = &mut capture => panic!("pipe-holding projection helper completed before outer cancellation"),
                pid = wait_for_stalled_helper_pid(&descendant_file) => {
                    pid.expect("pipe-holding projection descendant must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, descendant)
        };
        assert!(
            result.is_err(),
            "outer deadline must cancel projection with a pipe-holding descendant"
        );
        assert!(
            wait_for_stalled_helper_reap(descendant).await,
            "outer cancellation left a projection pipe descendant alive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projection_helper_closes_stdin_before_waiting_for_eof() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("eof-subagent-projection-helper");
        let response = directory.path().join("eof-subagent-projection-response");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/session/subagents/eof.jsonl",
            &child_transcript("projection fixture"),
            None,
        );
        let mut request = encode_subagent_projection_request_header(&source)
            .expect("encode projection request")
            .to_vec();
        request.extend_from_slice(&source.bytes);
        std::fs::write(
            &response,
            run_subagent_projection_helper(request).expect("build projection helper response"),
        )
        .expect("write projection helper response");
        let response_path = response.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\n/bin/cat >/dev/null\nexec /bin/cat '{response_path}'\n"),
        )
        .expect("write EOF-sensitive projection helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let (_, _, partial, turn_count, _) =
            with_subagent_projection_helper_program(helper, async {
                safe_content_projection_until(&source, Instant::now() + Duration::from_secs(5))
                    .await
            })
            .await
            .expect("projection helper must receive EOF before the parent waits for it");
        assert!(!partial, "complete fixture must remain complete");
        assert!(
            turn_count > 0,
            "projection helper must return its response after EOF"
        );
    }

    #[tokio::test]
    async fn in_process_parent_validation_delay_respects_the_discovery_deadline() {
        let deadline = Instant::now() + Duration::from_millis(20);
        let result = with_subagent_parent_validation_delay(Duration::from_millis(80), async {
            subagent_discovery_test_pause_before_parent_validation(deadline).await;
            ensure_before_discovery_deadline(deadline)
        })
        .await;
        let error = result.expect_err("the in-process parent-validation delay must time out");
        assert!(
            format!("{error:#}").contains("deadline"),
            "the delay must retain the bounded-discovery error classification: {error:#}"
        );
    }

    #[test]
    fn empty_or_metadata_only_child_content_is_partial() {
        for bytes in [b"".as_slice(), br#"{"type":"system","message":"metadata"}"#] {
            let (malformed, partial) =
                subagent_source_completeness(bytes, None).expect("classify child content");
            assert_eq!(malformed, 0);
            assert!(
                partial,
                "child content without a normalized turn is incomplete"
            );
        }
    }

    #[test]
    fn parent_side_child_validation_observes_the_absolute_deadline() {
        let error =
            subagent_source_completeness(&child_transcript("late child"), Some(Instant::now()))
                .expect_err("expired validation deadline must fail");
        assert!(error.to_string().contains("deadline"));
    }

    async fn test_store() -> (tempfile::TempDir, DatabaseConnection, PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let storage_root = directory.path().join(".libra");
        std::fs::create_dir_all(storage_root.join("objects")).expect("objects dir");
        // The V2 source capability and checkpoint object-index writer must
        // agree on one physical repository database. An in-memory connection
        // plus an empty on-disk marker lets the latter emit stranded repair
        // markers, which is not representative of a real capture.
        let database = storage_root.join(crate::utils::util::DATABASE);
        let conn = crate::internal::db::create_database(&database.to_string_lossy())
            .await
            .expect("repository database");
        ConfigKv::set_with_conn(&conn, "libra.repoid", "subagent-content-test", false)
            .await
            .expect("seed repository identity");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
        ))
        .await
        .expect("minimal ai_thread FK target");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('parent-session', 'claude_code', 'provider-session',
                       'active', ?, '{}', '{}', 1, 1, 1,
                       'subagent-content-test', '', NULL, NULL, 'scoped')",
            [directory.path().to_string_lossy().into_owned().into()],
        ))
        .await
        .expect("parent session");
        (directory, conn, storage_root)
    }

    async fn scoped_test_store() -> (
        tempfile::TempDir,
        DatabaseConnection,
        PathBuf,
        CaptureScope,
        String,
    ) {
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope {
            repo_id: "subagent-content-test".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("subagent-content-workspace".to_string()),
            workspace_fence: Some(9),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/subagent-content-workspace', 'agent',
                       'active', 'subagent-content-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live subagent workspace");
        let parent_session_id = "scoped-parent-session".to_string();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, 'claude_code', 'scoped-provider-session',
                       'active', '/repo', '{}', '{}', 1, 1, 1,
                       ?, ?, ?, ?, 'scoped')",
            [
                parent_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed scoped subagent parent session");
        (directory, conn, storage_root, scope, parent_session_id)
    }

    fn child_transcript(text: &str) -> Vec<u8> {
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "user",
                "uuid": "child-user",
                "message": {"role": "user", "content": text}
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "child-assistant",
                "message": {
                    "role": "assistant",
                    "content": "done",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }
            })
        )
        .into_bytes()
    }

    /// Direct SQL-sink tests bypass discovery, so they need the same opaque
    /// durable shape that the production capture boundary derives before
    /// calling reservation/finalization.
    fn durable_fixture(
        provider_kind: &str,
        transient_source_key: &str,
        bytes: &[u8],
        stable_subagent_id: Option<&str>,
    ) -> DiscoveredSubagentContent {
        let mut source = DiscoveredSubagentContent::fixture(
            provider_kind,
            transient_source_key,
            bytes,
            stable_subagent_id,
        );
        source.source_key = format!(
            "source/subagent-hmac-v2/{}",
            hex::encode(Sha256::digest(transient_source_key.as_bytes()))
        );
        source
    }

    async fn durable_source_for_test_capture(
        conn: &DatabaseConnection,
        storage_root: &Path,
        parent_root: &Path,
        source: &DiscoveredSubagentContent,
    ) -> DiscoveredSubagentContent {
        let scope = CaptureScope::main_for_connection(conn)
            .await
            .expect("test capture scope");
        let preimage = subagent_source_preimage(&source.provider_kind, &source.source_key, None)
            .expect("test source preimage");
        let commitment = derive_capture_source_commitment_in_scope_until(
            conn,
            &scope,
            storage_root,
            parent_root,
            CaptureSourceCommitmentDomain::SubagentSourceV2,
            &preimage,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("derive test V2 source commitment");
        let mut durable = source.clone();
        durable.legacy_source_key = Some(
            source_key_for_incarnation(&source.source_key, None).expect("test legacy source proof"),
        );
        durable.source_key = commitment;
        durable
    }

    #[tokio::test]
    async fn raw_subagent_locator_cannot_reach_reservation_sink() {
        let (_directory, conn, _storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/raw-locator.jsonl",
            &child_transcript("raw source must stay transient"),
            None,
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "test-digest",
                checkpoint_id: "test-checkpoint",
                owner: "test-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("raw subagent locator must be rejected before reservation");
        assert!(format!("{error:#}").contains("V2 source commitment"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim",
            )
            .await,
            0,
            "a rejected raw source must not create a durable claim"
        );
    }

    #[tokio::test]
    async fn legacy_sha_subagent_key_cannot_reach_reservation_sink() {
        let (_directory, conn, _storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "source/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            &child_transcript("legacy SHA source must stay read-only"),
            None,
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &source,
            ReservationAttempt {
                content_digest: "test-digest",
                checkpoint_id: "test-checkpoint",
                owner: "test-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("legacy SHA source key must be rejected before reservation");
        assert!(format!("{error:#}").contains("V2 source commitment"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim",
            )
            .await,
            0,
            "a legacy SHA key must not create a durable claim"
        );
    }

    async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("scalar query")
        .expect("scalar row")
        .try_get_by("n")
        .expect("scalar value")
    }

    fn test_mutation_deadline() -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_add(5_000),
        )
    }

    fn expired_test_mutation_deadline() -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() - Duration::from_millis(1),
            Utc::now().timestamp_millis().saturating_sub(1),
        )
    }

    /// A child writer can reserve its source before a workspace is released.
    /// Once that lease is stale, it must not create a marker, resolve a link,
    /// append child catalog rows, or clean up the reservation that a current
    /// owner may need to recover.
    #[tokio::test]
    async fn expired_workspace_scope_blocks_subagent_marker_link_final_and_release_mutations() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/scope-expiry.jsonl",
            &child_transcript("captured while workspace lease was live"),
            Some("scope-expiry-child"),
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-scope-expiry-checkpoint";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "scope-expiry-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("reserve child source while workspace lease is live");
        let ReservationOutcome::Reserved {
            fence_token,
            revision,
        } = reservation
        else {
            panic!("expected a live child reservation");
        };

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0 WHERE workspace_id = 'subagent-content-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after child reservation");

        let marker = TracesInflightMarker::new(&parent_session_id, checkpoint_id, 1);
        let marker_generation = marker
            .generation
            .as_deref()
            .expect("marker generation")
            .to_string();
        let error = register_traces_write_attempt_with_scope(
            &conn,
            &scope,
            &marker,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not register a child traces marker");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight' AND target = 'scoped-parent-session'
                   AND key = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "expired scope must not create a marker"
        );

        let error = refresh_current_link(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not refresh a child link");
        assert!(format!("{error:#}").contains("workspace lease"));

        let plan = ContentCommitPlan {
            capture_scope: scope.clone(),
            parent_session_id: parent_session_id.clone(),
            provider_kind: source.provider_kind.clone(),
            source_key: source.source_key.clone(),
            content_digest,
            checkpoint_id: checkpoint_id.to_string(),
            owner: "scope-expiry-owner".to_string(),
            fence_token,
            revision,
            parent_commit: None,
            source_channel: "live".to_string(),
            partial,
            stable_subagent_id: source.stable_subagent_id.clone(),
            created_at: 1,
            deadline: test_mutation_deadline(),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin child final transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "subagent-scope-expiry-commit".to_string(),
                    tree_oid: "subagent-scope-expiry-tree".to_string(),
                    metadata_blob_oid: "subagent-scope-expiry-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired scope must reject the final child transaction");
        assert!(format!("{error:#}").contains("workspace lease"));
        txn.rollback()
            .await
            .expect("roll back rejected child transaction");

        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "scope-expiry-owner",
                fence_token,
            },
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not release a child reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "final child checkpoint row must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                 WHERE checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "child revision must not be appended"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE content_checkpoint_id = 'subagent-scope-expiry-checkpoint'",
            )
            .await,
            0,
            "child link must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'scope-expiry-owner'",
            )
            .await,
            1,
            "expired cleanup must leave the source reservation untouched"
        );
        let error = clear_non_cleanup_traces_inflight_marker_with_scope(
            &conn,
            &scope,
            &parent_session_id,
            checkpoint_id,
            &marker_generation,
            test_mutation_deadline(),
        )
        .await
        .expect_err("expired scope must not clear a child marker");
        assert!(format!("{error:#}").contains("workspace lease"));
    }

    /// Once the child slice expires, no new V2 durable claim, marker, link,
    /// checkpoint, or revision may appear. Releasing the exact reservation
    /// created before expiry is deliberately different: it is a bounded
    /// cleanup write that prevents the old owner from stranding a lease.
    #[tokio::test]
    async fn absolute_deadline_blocks_new_subagent_v2_writes_but_allows_owned_release() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/deadline.jsonl",
            &child_transcript("absolute deadline fixture"),
            Some("deadline-child"),
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-absolute-deadline-checkpoint";
        let expired = expired_test_mutation_deadline();

        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            expired,
        )
        .await
        .expect_err("expired capture must not create a V2 source claim");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "an expired capture must not create a V2 claim"
        );

        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before deadline expiry");
        let ReservationOutcome::Reserved {
            fence_token,
            revision,
        } = reservation
        else {
            panic!("expected provisional reservation");
        };

        let marker = TracesInflightMarker::new(&parent_session_id, checkpoint_id, 1);
        let error = register_traces_write_attempt_with_scope(&conn, &scope, &marker, expired)
            .await
            .expect_err("expired capture must not create a traces marker");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'
                   AND target = 'scoped-parent-session'
                   AND key = 'subagent-absolute-deadline-checkpoint'",
            )
            .await,
            0,
            "expired capture must not create a V2 traces marker"
        );

        let error = refresh_current_link(&conn, &scope, &parent_session_id, &source, expired)
            .await
            .expect_err("expired capture must not refresh a child link");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());

        let plan = ContentCommitPlan {
            capture_scope: scope.clone(),
            parent_session_id: parent_session_id.clone(),
            provider_kind: source.provider_kind.clone(),
            source_key: source.source_key.clone(),
            content_digest,
            checkpoint_id: checkpoint_id.to_string(),
            owner: "deadline-owner".to_string(),
            fence_token,
            revision,
            parent_commit: None,
            source_channel: "live".to_string(),
            partial,
            stable_subagent_id: source.stable_subagent_id.clone(),
            created_at: 1,
            deadline: expired,
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin expired finalization transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "subagent-absolute-deadline-commit".to_string(),
                    tree_oid: "subagent-absolute-deadline-tree".to_string(),
                    metadata_blob_oid: "subagent-absolute-deadline-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired capture must not finalize a child checkpoint");
        assert!(error.downcast_ref::<SubagentCaptureDeadline>().is_some());
        txn.rollback()
            .await
            .expect("roll back unused expired finalization transaction");

        for (table, predicate) in [
            (
                "agent_checkpoint",
                "checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
            (
                "agent_subagent_content_revision",
                "checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
            (
                "agent_subagent_link",
                "content_checkpoint_id = 'subagent-absolute-deadline-checkpoint'",
            ),
        ] {
            let count = scalar(
                &conn,
                &format!("SELECT COUNT(*) AS n FROM {table} WHERE {predicate}"),
            )
            .await;
            assert_eq!(count, 0, "expired capture must not write {table}");
        }

        release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "deadline-owner",
                fence_token,
            },
            expired,
        )
        .await
        .expect("expired capture may release its own provisional reservation");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'idle' AND owner IS NULL",
            )
            .await,
            1,
            "cleanup must retire only the existing owned reservation"
        );
    }

    /// A still-live process deadline may already be expired in SQLite's
    /// authoritative wall-clock domain. Recovery cleanup must retain that
    /// original fence rather than re-anchor it to a fresh 250ms grace after
    /// the reservation DML has run.
    #[tokio::test]
    async fn subagent_owned_release_rejects_expired_live_sqlite_deadline_at_final_authorization() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/release-final-sqlite-deadline.jsonl",
            &child_transcript("release final SQLite deadline fixture"),
            None,
        );
        let checkpoint_id = "subagent-release-final-sqlite-deadline-checkpoint";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "release-final-sqlite-deadline-digest",
                checkpoint_id,
                owner: "release-final-sqlite-deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before final-authorization deadline test");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "release-final-sqlite-deadline-owner",
                fence_token,
            },
            deadline,
        )
        .await
        .expect_err("expired SQLite authorization must roll back a reservation release");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final authorization must preserve the capture-deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'release-final-sqlite-deadline-owner'",
            )
            .await,
            1,
            "an expired final SQLite deadline must leave the owned reservation intact"
        );
    }

    /// Recovery may release an existing reservation only during its short
    /// grace. A real independent SQLite `BEGIN EXCLUSIVE` lock must not keep
    /// the cancelled writer queued and release the reservation after the lock
    /// holder goes away.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_owned_release_recovery_grace_bounds_exclusive_sqlite_lock_without_late_write()
    {
        let (_directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/release-recovery-lock.jsonl",
            &child_transcript("release recovery lock fixture"),
            None,
        );
        let checkpoint_id = "subagent-release-recovery-lock-checkpoint";
        let owner = "release-recovery-lock-owner";
        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "release-recovery-lock-digest",
                checkpoint_id,
                owner,
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed provisional reservation before recovery-lock test");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA journal_mode = DELETE".to_string(),
        ))
        .await
        .expect("force rollback-journal mode for release recovery-lock regression");
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent release recovery-lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent release recovery-lock contender");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "PRAGMA journal_mode = DELETE".to_string(),
            ))
            .await
            .expect("force rollback-journal mode on release recovery-lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive release recovery lock");

        let started = Instant::now();
        let release = tokio::time::timeout(
            Duration::from_secs(2),
            release_reservation(
                &contender,
                &scope,
                ReservationRelease {
                    parent_session_id: &parent_session_id,
                    source: &source,
                    checkpoint_id,
                    owner,
                    fence_token,
                },
                expired_test_mutation_deadline(),
            ),
        )
        .await
        .expect("recovery release must respect its short writer-acquisition grace");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "ROLLBACK".to_string(),
            ))
            .await
            .expect("release exclusive recovery lock");

        let error = release.expect_err("exclusive lock must exhaust the recovery release grace");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "recovery writer timeout must preserve the capture-deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "recovery release must not wait for SQLite's default busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'release-recovery-lock-owner'",
            )
            .await,
            1,
            "a cancelled recovery writer wait must not release the reservation after lock release"
        );
    }

    /// The paired SQLite half is the authoritative last-write fence. A live
    /// process-side Instant with an already-expired SQLite deadline must roll
    /// back the claim written before final authorization, rather than merely
    /// rejecting at method entry.
    #[tokio::test]
    async fn subagent_claim_final_authorization_rejects_expired_sqlite_half_after_dml() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-sqlite-deadline.jsonl",
            &child_transcript("final SQLite deadline fixture"),
            None,
        );
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );

        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "final-sqlite-deadline-digest",
                checkpoint_id: "subagent-final-sqlite-deadline-checkpoint",
                owner: "final-sqlite-deadline-owner",
            },
            &UnchangedDurabilityProof::default(),
            deadline,
        )
        .await
        .expect_err("expired SQLite final authorization must roll back a new claim");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final authorization must retain the capture deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "failed final authorization must leave no newly durable V2 claim"
        );
    }

    /// The absolute deadline must also bound the SQLite writer wait itself.
    /// A separate connection holds the real file's writer slot; the contender
    /// may not wake up later and publish a V2 claim after its capture budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_v2_claim_deadline_cancels_contended_sqlite_writer_without_side_effect() {
        use sea_orm::TransactionTrait;

        let (_directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent SQLite lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent SQLite deadline contender");
        let held = holder.begin().await.expect("begin lock-holder transaction");
        held.execute_raw(Statement::from_sql_and_values(
            held.get_database_backend(),
            "INSERT INTO config_kv (key, value, encrypted) VALUES (?, 'hold', 0)",
            ["subagent-content-deadline-lock".into()],
        ))
        .await
        .expect("take SQLite writer lock");

        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/deadline-lock.jsonl",
            &child_transcript("deadline lock fixture"),
            None,
        );
        let result = reserve_source(
            &contender,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: "deadline-lock-digest",
                checkpoint_id: "subagent-deadline-lock-checkpoint",
                owner: "deadline-lock-owner",
            },
            &UnchangedDurabilityProof::default(),
            CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_millis(30),
                Utc::now().timestamp_millis().saturating_add(30),
            ),
        )
        .await;
        held.rollback()
            .await
            .expect("release SQLite writer lock after deadline");
        let error = result.expect_err("contended reservation must stop at its deadline");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "writer-lock expiry must retain the subagent deadline classification: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND content_schema_version = 2",
            )
            .await,
            0,
            "a canceled writer wait must not publish a delayed V2 claim"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_capture_deadline_bounds_initial_reads_under_exclusive_sqlite_lock() {
        let (directory, conn, storage_root, scope, parent_session_id) = scoped_test_store().await;
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA journal_mode = DELETE".to_string(),
        ))
        .await
        .expect("force rollback-journal mode for scoped capture deadline regression");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_session SET working_dir = ? WHERE session_id = ?",
            [
                directory.path().to_string_lossy().into_owned().into(),
                parent_session_id.clone().into(),
            ],
        ))
        .await
        .expect("point scoped parent at the test worktree");
        let database = storage_root.join(crate::utils::util::DATABASE);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            &database.to_string_lossy(),
            Duration::from_millis(50),
        )
        .await
        .expect("open scoped capture deadline lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "PRAGMA journal_mode = DELETE".to_string(),
            ))
            .await
            .expect("force rollback-journal mode on scoped capture deadline lock holder");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive scoped capture deadline lock");

        let sources = [DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/scoped-read-deadline.jsonl",
            &child_transcript("scoped read deadline fixture"),
            None,
        )];
        let started = Instant::now();
        let capture = tokio::time::timeout(
            Duration::from_secs(2),
            capture_discovered_subagent_contents_with_scope(
                &conn,
                &scope,
                &storage_root,
                &parent_session_id,
                &sources,
                "live",
                Some(CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_millis(30),
                    Utc::now().timestamp_millis().saturating_add(30),
                )),
            ),
        )
        .await
        .expect("scoped capture initial reads must honor their deadline");
        holder
            .execute_raw(Statement::from_string(
                holder.get_database_backend(),
                "ROLLBACK".to_string(),
            ))
            .await
            .expect("release exclusive scoped capture deadline lock");

        let error = capture.expect_err("locked scoped capture must stop at its deadline");
        assert!(
            capture_deadline_exhausted(&error),
            "exclusive initial read lock must retain the subagent deadline classification: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "scoped capture initial reads must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        for (sink, sql) in [
            (
                "subagent claim",
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            ),
            (
                "traces marker",
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight' AND target = 'scoped-parent-session'",
            ),
            (
                "checkpoint catalog row",
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE session_id = 'scoped-parent-session' AND scope = 'subagent'",
            ),
            (
                "traces ref",
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
            ),
        ] {
            assert_eq!(
                scalar(&conn, sql).await,
                0,
                "a cancelled scoped capture read must not publish a delayed {sink}"
            );
        }
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_subagent_reservation_and_release() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-scope-fence.jsonl",
            &child_transcript("final scope fence"),
            None,
        );
        let (transcript, _, partial, _) =
            safe_content_projection(&source).expect("project redacted child content");
        let content_digest =
            subagent_content_identity_digest(&transcript, partial, source.malformed_lines);
        let checkpoint_id = "subagent-final-scope-fence-checkpoint";

        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_claim_insert
             AFTER INSERT ON agent_subagent_content_claim
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install reservation scope-expiry trigger");
        let error = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "final-scope-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects the initial reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'",
            )
            .await,
            0,
            "the expired lease rolls back the initial reservation"
        );
        conn.execute_unprepared("DROP TRIGGER expire_subagent_scope_after_claim_insert")
            .await
            .expect("remove reservation scope-expiry trigger");

        let reservation = reserve_source(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id,
                owner: "final-scope-owner",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("reserve while the lease is live");
        let ReservationOutcome::Reserved { fence_token, .. } = reservation else {
            panic!("expected a live child reservation");
        };

        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_claim_update
             AFTER UPDATE ON agent_subagent_content_claim
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install release scope-expiry trigger");
        let error = release_reservation(
            &conn,
            &scope,
            ReservationRelease {
                parent_session_id: &parent_session_id,
                source: &source,
                checkpoint_id,
                owner: "final-scope-owner",
                fence_token,
            },
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects the reservation release");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE parent_session_id = 'scoped-parent-session'
                   AND state = 'reserved' AND owner = 'final-scope-owner'",
            )
            .await,
            1,
            "the expired lease rolls back the reservation release"
        );
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_subagent_link_refresh() {
        let (_directory, conn, _storage_root, scope, parent_session_id) = scoped_test_store().await;
        let source = durable_fixture(
            "claude_code",
            "project/provider-session/subagents/final-link-scope-fence.jsonl",
            &child_transcript("link final scope fence"),
            Some("stable-final-scope-child"),
        );
        let content_checkpoint_id = "subagent-final-scope-content";
        let boundary_checkpoint_id = "subagent-final-scope-boundary";
        for (checkpoint_id, stable_id) in [
            (content_checkpoint_id, None),
            (boundary_checkpoint_id, Some("stable-final-scope-child")),
        ] {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_checkpoint (
                    checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                    tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                    subagent_session_id, description, created_at
                 ) VALUES (?, ?, NULL, 'subagent', NULL, ?, ?, ?, NULL, ?,
                           'subagent scope-fence fixture', 1)",
                [
                    checkpoint_id.into(),
                    parent_session_id.clone().into(),
                    format!("tree-{checkpoint_id}").into(),
                    format!("metadata-{checkpoint_id}").into(),
                    format!("traces-{checkpoint_id}").into(),
                    stable_id.map(str::to_string).into(),
                ],
            ))
            .await
            .expect("seed subagent checkpoint fixture");
        }
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision_cursor, current_revision, current_checkpoint_id, current_digest,
                state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, sync_revision, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 1, 1, ?, 'digest', 'idle', NULL, NULL, NULL, NULL,
                       1, 1, 1, 1)",
            [
                parent_session_id.clone().into(),
                source.provider_kind.clone().into(),
                source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                content_checkpoint_id.into(),
            ],
        ))
        .await
        .expect("seed current subagent content claim");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id,
                stable_subagent_id, sync_revision, created_at, updated_at
             ) VALUES (?, ?, 'unresolved', NULL, ?, 1, 1, 1)",
            [
                content_checkpoint_id.into(),
                parent_session_id.clone().into(),
                "stable-final-scope-child".into(),
            ],
        ))
        .await
        .expect("seed unresolved subagent link");
        conn.execute_unprepared(
            "CREATE TRIGGER expire_subagent_scope_after_link_update
             AFTER UPDATE ON agent_subagent_link
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'subagent-content-workspace';
             END",
        )
        .await
        .expect("install link scope-expiry trigger");

        let error = refresh_current_link(
            &conn,
            &scope,
            &parent_session_id,
            &source,
            test_mutation_deadline(),
        )
        .await
        .expect_err("post-write expiry rejects link refresh");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE content_checkpoint_id = 'subagent-final-scope-content'
                   AND link_state = 'unresolved' AND boundary_checkpoint_id IS NULL",
            )
            .await,
            1,
            "the expired lease rolls back the subagent link refresh"
        );
    }

    async fn seed_boundary(
        conn: &DatabaseConnection,
        checkpoint_id: &str,
        stable_id: Option<&str>,
    ) {
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, 'parent-session', NULL, 'subagent', NULL,
                       ?, ?, ?, NULL, ?, 'subagent boundary', 1)",
            [
                checkpoint_id.into(),
                format!("tree-{checkpoint_id}").into(),
                format!("metadata-{checkpoint_id}").into(),
                format!("traces-{checkpoint_id}").into(),
                stable_id.map(str::to_string).into(),
            ],
        ))
        .await
        .expect("boundary row");
    }

    #[test]
    fn fixture_source_identity_rejects_basename_escape() {
        let source =
            DiscoveredSubagentContent::fixture("claude_code", "../child.jsonl", b"{}\n", None);
        assert!(validate_source_identity(&source.provider_kind, &source.source_key).is_err());
    }

    #[test]
    fn restored_capture_incarnation_namespaces_source_identity() {
        let source = format!("source/sha256/{}", "a".repeat(64));
        assert_eq!(
            source_key_for_incarnation(&source, None).expect("legacy source identity"),
            source
        );
        let first = source_key_for_incarnation(&source, Some(&"1".repeat(32)))
            .expect("first restored incarnation");
        let second = source_key_for_incarnation(&source, Some(&"2".repeat(32)))
            .expect("second restored incarnation");
        assert_ne!(first, source);
        assert_ne!(first, second);
        assert!(first.starts_with("source/sha256/"));
        assert_eq!(first.len(), "source/sha256/".len() + 64);
    }

    #[test]
    fn claude_subagent_attribution() {
        let parent = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "name": "Task", "input": {"prompt": "child"}},
                        {"type": "tool_use", "name": "Write", "input": {"file_path": "src/parent.rs"}}
                    ],
                    "usage": {"input_tokens": 10, "output_tokens": 4}
                }
            })
        );
        let child = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "name": "Edit", "input": {"file_path": "src/child.rs"}}
                    ],
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }
            })
        );
        let adapter = ClaudeCodeObservedAgent::new();
        assert!(adapter.as_subagent_aware_extractor().is_some());
        let aggregate = adapter
            .extract_parent_and_subagents(parent.as_bytes(), &[child.as_bytes()])
            .expect("multi-source extraction");
        let paths = aggregate
            .modified_files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["src/parent.rs", "src/child.rs"]);
        assert_eq!(aggregate.aggregate_usage.input_tokens, 13);
        assert_eq!(
            aggregate
                .subagent_usage
                .expect("child usage attributed")
                .input_tokens,
            3
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn claude_subagent_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "abcdef00-0000-0000-0000-000000000001";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        let target = home.path().join("target.jsonl");
        std::fs::write(&target, child_transcript("child")).expect("target");
        symlink(&target, directory.join("child.jsonl")).expect("symlink");
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("symlink must fail closed");
        assert!(format!("{error:#}").contains("symlink"));
        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn claude_subagent_discovery_enforces_budget_and_persists_only_opaque_identity() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo/customer-secret");
        let session_id = "abcdef00-0000-0000-0000-000000000002";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        let bytes = child_transcript("child");
        std::fs::write(directory.join("alice-secret.jsonl"), &bytes).expect("child transcript");

        let too_small = u64::try_from(bytes.len())
            .expect("fixture size")
            .saturating_sub(1);
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            too_small,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("configured byte budget must be enforced");
        assert!(format!("{error:#}").contains("input budget"));

        let discovery = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            u64::try_from(bytes.len()).expect("fixture size"),
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect("bounded discovery");
        assert_eq!(discovery.bytes_read, bytes.len() as u64);
        assert_eq!(discovery.sources.len(), 1);
        let source_key = &discovery.sources[0].source_key;
        assert!(source_key.starts_with("source/sha256/"));
        assert_eq!(source_key.len(), "source/sha256/".len() + 64);
        assert!(!source_key.contains("customer-secret"));
        assert!(!source_key.contains("alice-secret"));
        assert!(!source_key.contains(session_id));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn claude_subagent_discovery_supports_legacy_safe_session_components() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "Legacy.session_01";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        std::fs::write(directory.join("child.jsonl"), child_transcript("child"))
            .expect("child transcript");

        let discovery = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect("legacy safe session id must retain child discovery");
        assert_eq!(discovery.sources.len(), 1);
        assert!(claude_session_id_is_safe_path_component("legacy_01"));
        assert!(claude_session_id_is_safe_path_component("legacy.session"));
        assert!(!claude_session_id_is_safe_path_component(".."));
        assert!(!claude_session_id_is_safe_path_component("legacy/session"));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn claude_subagent_discovery_counts_non_json_entries_toward_directory_bound() {
        let home = tempfile::tempdir().expect("home");
        let old_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: this test is serial and restores the process environment.
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let cwd = Path::new("/repo");
        let session_id = "abcdef00-0000-0000-0000-000000000003";
        let directory = home
            .path()
            .join(".claude/projects")
            .join(claude_project_slug(cwd))
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&directory).expect("subagents dir");
        for index in 0..=MAX_SUBAGENT_DIRECTORY_ENTRIES {
            std::fs::write(directory.join(format!("noise-{index}.txt")), b"").expect("noise entry");
        }
        let error = discover_claude_subagent_contents(
            cwd,
            session_id,
            None,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .expect_err("non-JSON entries must count toward enumeration limit");
        assert!(format!("{error:#}").contains("entry safety limit"));

        match old_home {
            Some(value) => {
                // SAFETY: serial test restoration.
                unsafe { std::env::set_var("LIBRA_TEST_HOME", value) }
            }
            None => {
                // SAFETY: serial test restoration.
                unsafe { std::env::remove_var("LIBRA_TEST_HOME") }
            }
        }
    }

    #[tokio::test]
    async fn subagent_content_repeat_single_visible_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        let first = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("first capture");
        let second = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("repeat capture");
        assert_eq!(first.checkpoints_written, 1);
        assert_eq!(second.skipped_unchanged, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE current_revision = 1"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn compatibility_wrapper_none_deadline_bounds_final_checkpoint_append() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/fallback-deadline.jsonl",
            &child_transcript("fallback deadline"),
            None,
        );
        let observed = Arc::new(std::sync::Mutex::new(None));
        let capture = TEST_SUBAGENT_FORCE_FALLBACK_APPEND_DEADLINE
            .scope(
                true,
                TEST_SUBAGENT_FINAL_APPEND_DEADLINE.scope(
                    Arc::clone(&observed),
                    capture_discovered_subagent_contents(
                        &conn,
                        &storage_root,
                        "parent-session",
                        &[source],
                        "live",
                        None,
                    ),
                ),
            )
            .await;
        let error = capture.expect_err(
            "the embedded unit-test host reaches the bounded append but has no private helper",
        );
        assert!(
            format!("{error:#}").contains("checkpoint object-I/O helper is unavailable"),
            "the regression must reach HistoryManager's deadline-bound append: {error:#}"
        );
        assert!(
            observed
                .lock()
                .expect("lock final append deadline observation")
                .expect("final checkpoint append must be reached")
                .is_some(),
            "a wrapper call with no caller deadline must pass its fallback deadline to history"
        );
    }

    #[tokio::test]
    async fn unchanged_children_share_one_global_durability_traversal() {
        let (_directory, conn, storage_root) = test_store().await;
        let sources = vec![
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/one.jsonl",
                &child_transcript("one"),
                None,
            ),
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/two.jsonl",
                &child_transcript("two"),
                None,
            ),
        ];
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &sources,
            "live",
            None,
        )
        .await
        .expect("initial child capture");

        let (replay, traversals) =
            history::count_checkpoint_snapshot_verifications(capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &sources,
                "live",
                None,
            ))
            .await;
        let replay = replay.expect("unchanged child replay");
        assert_eq!(replay.skipped_unchanged, 2);
        assert_eq!(traversals, 1, "all unchanged children use one traces walk");
    }

    #[tokio::test]
    async fn subagent_content_identity_includes_partial_and_malformed_state() {
        let (_directory, conn, storage_root) = test_store().await;
        let clean = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/state.jsonl",
            &child_transcript("same projection"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&clean),
            "live",
            None,
        )
        .await
        .expect("capture clean source");

        let mut malformed = clean.clone();
        malformed.malformed_lines = 1;
        let changed = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[malformed],
            "live",
            None,
        )
        .await
        .expect("capture newly malformed source");
        assert_eq!(changed.checkpoints_written, 1);

        let restored = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[clean],
            "live",
            None,
        )
        .await
        .expect("capture source restored to clean");
        assert_eq!(restored.checkpoints_written, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            3
        );
    }

    #[tokio::test]
    async fn unchanged_replay_clears_expired_matching_reservation() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/expired.jsonl",
            &child_transcript("same"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("initial capture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim
                 SET state = 'reserved', attempt_digest = current_digest,
                     attempt_checkpoint_id = 'expired-attempt', owner = 'crashed',
                     lease_expires_at = {}, fence_token = 7",
                Utc::now().timestamp_millis() - 1
            ),
        ))
        .await
        .expect("seed expired matching reservation");
        let replay = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("unchanged replay");
        assert_eq!(replay.skipped_unchanged, 1);
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, attempt_digest, owner, fence_token
                 FROM agent_subagent_content_claim"
                    .to_string(),
            ))
            .await
            .expect("query claim")
            .expect("claim row");
        assert_eq!(row.try_get_by::<String, _>("state").expect("state"), "idle");
        assert_eq!(
            row.try_get_by::<Option<String>, _>("attempt_digest")
                .expect("attempt digest"),
            None
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner").expect("owner"),
            None
        );
        assert_eq!(row.try_get_by::<i64, _>("fence_token").expect("fence"), 8);
    }

    /// The expired-reservation probe first clears only an existing provisional
    /// row, then validates the unchanged leaf. Its final authorization must
    /// still retain the paired deadline that was live at dispatch; otherwise a
    /// stale SQLite half would let that post-DML cleanup commit.
    #[tokio::test]
    async fn expired_reservation_probe_rolls_back_cleanup_after_expired_final_sqlite_deadline() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/expired-final-deadline.jsonl",
            &child_transcript("same final deadline"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("seed an unchanged completed child before probe deadline test");

        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT source_key, current_digest FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("load durable source key and digest")
            .expect("seeded child claim");
        let source_key: String = row.try_get_by("source_key").expect("durable source key");
        let content_digest = row
            .try_get_by::<Option<String>, _>("current_digest")
            .expect("read completed child digest")
            .expect("completed child digest");
        let mut durable_source = source.clone();
        durable_source.source_key = source_key;
        durable_source.legacy_source_key = None;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_subagent_content_claim
             SET state = 'reserved', attempt_digest = current_digest,
                 attempt_checkpoint_id = 'expired-final-deadline-attempt',
                 owner = 'crashed-owner', lease_expires_at = ?, fence_token = 7",
            [Utc::now().timestamp_millis().saturating_sub(1).into()],
        ))
        .await
        .expect("seed expired matching reservation for deadline probe");

        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("resolve main capture scope");
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: &content_digest,
                checkpoint_id: "expired-final-deadline-probe",
                owner: "probe-owner",
            },
            &UnchangedDurabilityProof::default(),
            deadline,
        )
        .await
        .expect_err("expired final SQLite deadline must roll back the cleanup probe");
        assert!(
            error.downcast_ref::<SubagentCaptureDeadline>().is_some(),
            "final cleanup authorization must retain the deadline classification: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state, owner, fence_token FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("query claim after rejected cleanup probe")
            .expect("seeded child claim remains");
        assert_eq!(
            row.try_get_by::<String, _>("state").expect("state"),
            "reserved"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner").expect("owner"),
            Some("crashed-owner".to_string())
        );
        assert_eq!(row.try_get_by::<i64, _>("fence_token").expect("fence"), 7);
    }

    #[tokio::test]
    async fn unchanged_replay_verifies_ref_and_all_catalog_objects() {
        for damaged_field in [
            "metadata_blob_oid",
            "tree_oid",
            "traces_commit",
            "traces_ref",
            "descendant_object",
        ] {
            let (_directory, conn, storage_root) = test_store().await;
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/durable.jsonl",
                &child_transcript("durable"),
                None,
            );
            capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                std::slice::from_ref(&source),
                "live",
                None,
            )
            .await
            .expect("initial capture");
            if damaged_field == "traces_ref" {
                conn.execute_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "UPDATE reference SET `commit` = NULL
                     WHERE name = 'traces' AND kind = 'Branch'"
                        .to_string(),
                ))
                .await
                .expect("clear traces ref");
            } else if damaged_field == "descendant_object" {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        "SELECT checkpoint_id, traces_commit, tree_oid, metadata_blob_oid
                         FROM agent_checkpoint"
                            .to_string(),
                    ))
                    .await
                    .expect("query checkpoint identity")
                    .expect("checkpoint row");
                let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint");
                let traces_commit: String = row.try_get_by("traces_commit").expect("commit");
                let tree_oid: String = row.try_get_by("tree_oid").expect("tree");
                let metadata_blob_oid: String =
                    row.try_get_by("metadata_blob_oid").expect("metadata");
                let oid = history::checkpoint_leaf_durable_oids(
                    &conn,
                    &storage_root,
                    &checkpoint_id,
                    &traces_commit,
                    &tree_oid,
                    &metadata_blob_oid,
                )
                .await
                .expect("collect durable objects")
                .into_iter()
                .find(|oid| oid != &traces_commit && oid != &tree_oid && oid != &metadata_blob_oid)
                .expect("checkpoint has a descendant object");
                std::fs::remove_file(storage_root.join("objects").join(&oid[..2]).join(&oid[2..]))
                    .expect("remove descendant object");
            } else {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        format!("SELECT {damaged_field} AS oid FROM agent_checkpoint"),
                    ))
                    .await
                    .expect("query object oid")
                    .expect("checkpoint row");
                let oid: String = row.try_get_by("oid").expect("object oid");
                std::fs::remove_file(storage_root.join("objects").join(&oid[..2]).join(&oid[2..]))
                    .expect("remove checkpoint object");
            }
            let error = capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect_err("damaged durable identity must not replay as unchanged");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("libra agent doctor"),
                "{damaged_field}: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn supplied_catalog_validation_rejects_an_existing_corrupt_object() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/cloud-restore.jsonl",
            &child_transcript("cloud restore"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("capture cloud-restore durability fixture");
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id, traces_commit, tree_oid, metadata_blob_oid
                 FROM agent_checkpoint"
                    .to_string(),
            ))
            .await
            .expect("query checkpoint identity")
            .expect("checkpoint row");
        let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint");
        let traces_commit: String = row.try_get_by("traces_commit").expect("commit");
        let tree_oid: String = row.try_get_by("tree_oid").expect("tree");
        let metadata_blob_oid: String = row.try_get_by("metadata_blob_oid").expect("metadata");
        let spec = history::CheckpointDurabilitySpec {
            checkpoint_id: &checkpoint_id,
            traces_commit: &traces_commit,
            tree_oid: &tree_oid,
            metadata_blob_oid: &metadata_blob_oid,
        };
        let durable =
            history::checkpoint_rows_snapshot_durable_oids(&conn, &storage_root, &[spec], None)
                .await
                .expect("validate supplied checkpoint catalog");
        let damaged = durable
            .into_iter()
            .find(|oid| oid != &traces_commit && oid != &tree_oid && oid != &metadata_blob_oid)
            .expect("checkpoint has a descendant object");
        std::fs::write(
            storage_root
                .join("objects")
                .join(&damaged[..2])
                .join(&damaged[2..]),
            b"corrupt-existing-object",
        )
        .expect("corrupt an existing loose object");

        let error =
            history::checkpoint_rows_snapshot_durable_oids(&conn, &storage_root, &[spec], None)
                .await
                .expect_err("path existence must not satisfy restore durability");
        assert!(
            format!("{error:#}").contains("checkpoint"),
            "unexpected validation error: {error:#}"
        );
    }

    #[tokio::test]
    async fn subagent_content_replay_rejects_dangling_current_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("first capture");
        let checkpoint_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
            [checkpoint_id.into()],
        ))
        .await
        .expect("delete content catalog row");

        let error = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect_err("replay must not accept a dangling current leaf as unchanged");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("current leaf is incomplete"));
        assert!(rendered.contains("libra agent doctor"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn subagent_content_concurrent_same_source_single_append() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent.jsonl",
            &child_transcript("concurrent"),
            None,
        );
        let first_conn = conn.clone();
        let second_conn = conn.clone();
        let first_root = storage_root.clone();
        let second_root = storage_root.clone();
        let first_source = source.clone();
        let second_source = source.clone();
        let (first, second) = tokio::join!(
            async move {
                capture_discovered_subagent_contents(
                    &first_conn,
                    &first_root,
                    "parent-session",
                    &[first_source],
                    "live",
                    None,
                )
                .await
            },
            async move {
                capture_discovered_subagent_contents(
                    &second_conn,
                    &second_root,
                    "parent-session",
                    &[second_source],
                    "live",
                    None,
                )
                .await
            }
        );
        let first = first.expect("first concurrent capture");
        let second = second.expect("second concurrent capture");
        assert_eq!(first.checkpoints_written + second.checkpoints_written, 1);
        assert_eq!(
            first.skipped_unchanged
                + first.skipped_inflight
                + second.skipped_unchanged
                + second.skipped_inflight,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'subagent'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_concurrent_changed_digest_eventually_appends_both() {
        let (_directory, conn, storage_root) = test_store().await;
        let first_source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent-changed.jsonl",
            &child_transcript("first"),
            None,
        );
        let second_source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/concurrent-changed.jsonl",
            &child_transcript("second"),
            None,
        );
        let first_conn = conn.clone();
        let second_conn = conn.clone();
        let first_root = storage_root.clone();
        let second_root = storage_root.clone();
        let (first, second) = tokio::join!(
            async move {
                capture_discovered_subagent_contents(
                    &first_conn,
                    &first_root,
                    "parent-session",
                    &[first_source],
                    "live",
                    None,
                )
                .await
            },
            async move {
                capture_discovered_subagent_contents(
                    &second_conn,
                    &second_root,
                    "parent-session",
                    &[second_source],
                    "live",
                    None,
                )
                .await
            }
        );
        assert!(first.is_ok(), "first writer: {first:?}");
        assert!(second.is_ok(), "second writer: {second:?}");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE revision_cursor = 2 AND current_revision = 2 AND state = 'idle'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_waits_for_crashed_writer_lease_then_captures() {
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/crashed.jsonl",
            &child_transcript("final"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        let stale = reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: "stale-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "crashed-writer",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed crashed reservation");
        assert!(matches!(stale, ReservationOutcome::Reserved { .. }));
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim SET lease_expires_at = {}",
                Utc::now().timestamp_millis() + 40
            ),
        ))
        .await
        .expect("shorten crashed lease");
        let summary = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("capture after crashed lease expiry");
        assert_eq!(summary.checkpoints_written, 1);
        assert_eq!(summary.skipped_inflight, 0);
    }

    #[tokio::test]
    async fn subagent_content_live_reservation_deadline_returns_retryable_error_not_skip() {
        let (directory, conn, storage_root) = test_store().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("test capture scope");
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/busy.jsonl",
            &child_transcript("final"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
        let stale_checkpoint = uuid::Uuid::new_v4().to_string();
        reserve_source(
            &conn,
            &scope,
            "parent-session",
            &durable_source,
            ReservationAttempt {
                content_digest: "different-inflight-digest",
                checkpoint_id: &stale_checkpoint,
                owner: "live-writer",
            },
            &UnchangedDurabilityProof::default(),
            test_mutation_deadline(),
        )
        .await
        .expect("seed live reservation");
        let deadline = Some(CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_millis(30),
            Utc::now().timestamp_millis().saturating_add(30),
        ));
        let error = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            deadline,
        )
        .await
        .expect_err("busy source must not be reported as a successful skip");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("another writer"));
        assert!(rendered.contains("retry"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn subagent_content_lease_takeover_on_expiry() {
        let (directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/stale.jsonl",
            &child_transcript("stale-owner"),
            None,
        );
        let durable_source =
            durable_source_for_test_capture(&conn, &storage_root, directory.path(), &source).await;
        let (projected, _, _, _) = safe_content_projection(&source).expect("safe projection");
        let digest = hex::encode(Sha256::digest(projected.as_ref()));
        let now_ms = Utc::now().timestamp_millis();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                current_revision, current_checkpoint_id, current_digest, state,
                attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, NULL, NULL, 'reserved', ?, ?, ?, ?, 1, ?, ?)",
            [
                "parent-session".into(),
                durable_source.provider_kind.clone().into(),
                durable_source.source_key.clone().into(),
                SUBAGENT_CONTENT_SCHEMA_VERSION.into(),
                digest.into(),
                "stale-checkpoint".into(),
                "crashed-owner".into(),
                now_ms.saturating_sub(1).into(),
                now_ms.saturating_sub(SUBAGENT_CONTENT_LEASE_MS).into(),
                now_ms.saturating_sub(1).into(),
            ],
        ))
        .await
        .expect("seed expired reservation");

        let recovered = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "import",
            None,
        )
        .await
        .expect("take over expired reservation");
        assert_eq!(recovered.checkpoints_written, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE current_revision = 1 AND state = 'idle' AND fence_token = 2
                   AND owner IS NULL AND lease_expires_at IS NULL"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_changed_source_advances_source_revision_without_parent_link() {
        let (_directory, conn, storage_root) = test_store().await;
        let first = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("first"),
            None,
        );
        let second = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("second"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[first],
            "live",
            None,
        )
        .await
        .expect("first revision");
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[second],
            "live",
            None,
        )
        .await
        .expect("second revision");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim WHERE current_revision = 2"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE scope = 'subagent' AND parent_checkpoint_id IS NULL"
            )
            .await,
            2,
            "content revisions must not overload structural parent linkage"
        );
    }

    #[tokio::test]
    async fn subagent_content_prune_repoints_current_source_leaf() {
        let (_directory, conn, storage_root) = test_store().await;
        for text in ["first", "second"] {
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/child.jsonl",
                &child_transcript(text),
                None,
            );
            capture_discovered_subagent_contents(
                &conn,
                &storage_root,
                "parent-session",
                &[source],
                "live",
                None,
            )
            .await
            .expect("content revision");
        }
        let current_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(storage_root.join("objects"))),
            storage_root.clone(),
            Arc::new(conn.clone()),
            crate::internal::branch::TRACES_BRANCH,
        );
        // Drain the fixture captures before arming the per-repository slow
        // consumer. This is test setup, not the production fix: old prune
        // rebuilding enqueued four *new* repair markers and then immediately
        // fenced its own marker batch at the test cap of three. Holding a
        // consumer makes that historical race deterministic.
        ClientStorage::wait_for_background_tasks();
        let _slow_repair_consumer = install_test_object_index_faults(
            &storage_root.join(crate::utils::util::DATABASE),
            ObjectIndexTestFaults {
                consumer_delay: Some(Duration::from_secs(5)),
                ..ObjectIndexTestFaults::default()
            },
        );
        let sync_revision_before = scalar(
            &conn,
            "SELECT sync_revision AS n FROM agent_subagent_content_claim",
        )
        .await;
        let pruned = manager
            .prune_checkpoint_commits(&[current_id])
            .await
            .expect("prune current content revision");
        assert_eq!(pruned.removed_checkpoints, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT sync_revision AS n FROM agent_subagent_content_claim"
            )
            .await,
            sync_revision_before + 1,
            "pruning a current leaf must advance its cloud sync generation"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE current_revision = 1 AND current_checkpoint_id IS NOT NULL"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
        let rewritten = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT traces_commit, tree_oid FROM agent_checkpoint
                 WHERE scope = 'subagent' ORDER BY created_at ASC LIMIT 1"
                    .to_string(),
            ))
            .await
            .expect("read surviving rewritten checkpoint")
            .expect("surviving rewritten checkpoint");
        let rewritten_commit: String = rewritten
            .try_get_by("traces_commit")
            .expect("decode rewritten checkpoint commit");
        let rewritten_tree: String = rewritten
            .try_get_by("tree_oid")
            .expect("decode rewritten checkpoint tree");
        assert_eq!(
            scalar(
                &conn,
                &format!(
                    "SELECT COUNT(*) AS n FROM object_index
                     WHERE o_id IN ('{}', '{}')",
                    rewritten_commit, rewritten_tree
                ),
            )
            .await,
            2,
            "the prune transaction must index its reachable replacement commit and root tree"
        );
        let repair_marker_count = std::fs::read_dir(storage_root.join("object-index-repair"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .count();
        assert_eq!(
            repair_marker_count, 0,
            "prune rebuild must not publish repair markers that its deletion fence would self-observe"
        );
        let third = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("third"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[third],
            "live",
            None,
        )
        .await
        .expect("append after pruning current revision");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                 WHERE revision IN (1, 3)"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE revision_cursor = 3 AND current_revision = 3"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_content_reservation_blocks_prune_before_marker() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("current"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("current content");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "UPDATE agent_subagent_content_claim
                 SET state = 'reserved', attempt_digest = 'next',
                     attempt_checkpoint_id = 'attempt-next', owner = 'writer',
                     lease_expires_at = {}, updated_at = {}",
                Utc::now().timestamp_millis() + 60_000,
                Utc::now().timestamp_millis()
            ),
        ))
        .await
        .expect("seed reservation-before-marker window");
        let current_id: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT current_checkpoint_id FROM agent_subagent_content_claim".to_string(),
            ))
            .await
            .expect("current query")
            .expect("current row")
            .try_get_by("current_checkpoint_id")
            .expect("current checkpoint");
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(storage_root.join("objects"))),
            storage_root,
            Arc::new(conn),
            crate::internal::branch::TRACES_BRANCH,
        );
        let error = manager
            .prune_checkpoint_commits(&[current_id])
            .await
            .expect_err("live source reservation must block whole-chain prune");
        assert!(matches!(
            error.downcast_ref::<history::SubagentContentReservationPruneGuard>(),
            Some(history::SubagentContentReservationPruneGuard { .. })
        ));
    }

    #[tokio::test]
    #[serial]
    async fn later_source_failure_preserves_committed_child_progress() {
        let (_directory, conn, storage_root) = test_store().await;
        let sources = [
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/first.jsonl",
                &child_transcript("first"),
                None,
            ),
            DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/second.jsonl",
                &child_transcript("second"),
                None,
            ),
        ];
        let error = TEST_SUBAGENT_CONTENT_FAILPOINT
            .scope(
                Some("after_first_commit"),
                capture_discovered_subagent_contents(
                    &conn,
                    &storage_root,
                    "parent-session",
                    &sources,
                    "import",
                    None,
                ),
            )
            .await
            .expect_err("failure after the first durable child must surface");
        let progress = error
            .downcast_ref::<SubagentCaptureProgressError>()
            .expect("typed partial child progress");
        assert_eq!(progress.summary().checkpoints_written, 1);
        assert_eq!(progress.summary().discovered, 2);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    #[serial]
    async fn subagent_content_failure_injection_leaves_no_visible_leaf_or_marker() {
        for stage in ["after_reservation", "after_marker", "before_final_sql"] {
            let (_directory, conn, storage_root) = test_store().await;
            let source = DiscoveredSubagentContent::fixture(
                "claude_code",
                "project/provider-session/subagents/child.jsonl",
                &child_transcript(stage),
                None,
            );
            let error = TEST_SUBAGENT_CONTENT_FAILPOINT
                .scope(
                    Some(stage),
                    capture_discovered_subagent_contents(
                        &conn,
                        &storage_root,
                        "parent-session",
                        &[source],
                        "live",
                        None,
                    ),
                )
                .await
                .expect_err("injected failure must surface");
            assert!(format!("{error:#}").contains(stage));
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE scope = 'subagent'"
                )
                .await,
                0,
                "{stage}: no catalog leaf may become visible"
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
                )
                .await,
                0,
                "{stage}: no revision may commit"
            );
            assert_eq!(
                scalar(
                    &conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                     WHERE state = 'idle' AND current_revision = 0"
                )
                .await,
                1,
                "{stage}: reservation must be replayable"
            );
            let marker_count = scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'",
            )
            .await;
            if stage == "before_final_sql" {
                assert_eq!(
                    marker_count, 1,
                    "object-producing rejection must retain its durable cleanup job"
                );
                assert_eq!(
                    scalar(
                        &conn,
                        "SELECT COUNT(*) AS n FROM metadata_kv
                         WHERE scope = 'agent_traces_inflight'
                           AND json_extract(value, '$.cleanup_pending') = 1"
                    )
                    .await,
                    1
                );
            } else {
                assert_eq!(
                    marker_count, 0,
                    "{stage}: empty ordinary in-flight marker must be cleared"
                );
            }
        }
    }

    #[tokio::test]
    async fn subagent_content_surfaces_reservation_cleanup_failure() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("cleanup-failure"),
            None,
        );
        let error = TEST_SUBAGENT_CONTENT_FAILPOINT
            .scope(
                Some("after_reservation,before_release_reservation"),
                capture_discovered_subagent_contents(
                    &conn,
                    &storage_root,
                    "parent-session",
                    &[source],
                    "live",
                    None,
                ),
            )
            .await
            .expect_err("primary and cleanup failures must both surface");
        let message = format!("{error:#}");
        assert!(message.contains("after_reservation"));
        assert!(message.contains("release subagent content reservation"));
        assert!(message.contains("before_release_reservation"));
        assert!(message.contains("libra agent doctor"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                 WHERE state = 'reserved'"
            )
            .await,
            1,
            "the surfaced cleanup failure must describe the still-live reservation"
        );
    }

    #[tokio::test]
    async fn subagent_boundary_without_stable_id_remains_unresolved() {
        let (_directory, conn, storage_root) = test_store().await;
        seed_boundary(&conn, "boundary-without-id", None).await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("unresolved"),
            None,
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("content capture");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn subagent_unique_id_links_boundary_without_history_rewrite() {
        let (_directory, conn, storage_root) = test_store().await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("stable"),
            Some("stable-child-1"),
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("initial unresolved content");
        let original_traces: String = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT cp.traces_commit FROM agent_checkpoint cp
                 JOIN agent_subagent_content_claim c
                   ON c.current_checkpoint_id = cp.checkpoint_id"
                    .to_string(),
            ))
            .await
            .expect("traces query")
            .expect("content row")
            .try_get_by("traces_commit")
            .expect("traces commit");
        seed_boundary(&conn, "boundary-stable", Some("stable-child-1")).await;
        let repeated = capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            std::slice::from_ref(&source),
            "live",
            None,
        )
        .await
        .expect("link refresh");
        assert_eq!(repeated.skipped_unchanged, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1,
            "link refresh must not append content history"
        );
        let linked = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT l.link_state, l.boundary_checkpoint_id, cp.traces_commit
                 FROM agent_subagent_link l
                 JOIN agent_checkpoint cp ON cp.checkpoint_id = l.content_checkpoint_id"
                    .to_string(),
            ))
            .await
            .expect("link query")
            .expect("link row");
        assert_eq!(
            linked.try_get_by::<String, _>("link_state").expect("state"),
            "resolved"
        );
        assert_eq!(
            linked
                .try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                .expect("boundary"),
            Some("boundary-stable".to_string())
        );
        assert_eq!(
            linked
                .try_get_by::<String, _>("traces_commit")
                .expect("traces"),
            original_traces,
            "association must not rewrite immutable traces history"
        );
    }

    #[tokio::test]
    async fn deleting_boundary_preserves_content_as_unresolved() {
        let (_directory, conn, storage_root) = test_store().await;
        seed_boundary(&conn, "boundary-stable", Some("stable-child-1")).await;
        let source = DiscoveredSubagentContent::fixture(
            "claude_code",
            "project/provider-session/subagents/child.jsonl",
            &child_transcript("stable"),
            Some("stable-child-1"),
        );
        capture_discovered_subagent_contents(
            &conn,
            &storage_root,
            "parent-session",
            &[source],
            "live",
            None,
        )
        .await
        .expect("resolved content capture");

        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
            ["boundary-stable".into()],
        ))
        .await
        .expect("delete boundary checkpoint");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE link_state = 'unresolved' AND boundary_checkpoint_id IS NULL
                   AND stable_subagent_id = 'stable-child-1'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link
                 WHERE updated_at >= 1000000000000"
            )
            .await,
            1,
            "link creation, refresh, and boundary-delete trigger timestamps use milliseconds"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_link WHERE sync_revision = 2"
            )
            .await,
            1,
            "boundary deletion advances the explicit link sync generation"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_revision"
            )
            .await,
            1,
            "deleting association evidence must not delete content history"
        );
    }
}
