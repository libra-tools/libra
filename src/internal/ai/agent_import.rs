//! Historical external-agent transcript import orchestration (plan-20260713 M4 / DR-05).
//!
//! The command layer discovers and authorizes a source, then hands the held
//! [`TranscriptSource`] to this module. Raw provider bytes stay in memory only:
//! they are strictly parsed into typed turns, redacted field-by-field, and
//! re-serialized as an allowlist-only per-turn projection before persistence.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    capture_scope::CaptureScope,
    observed_agents::{
        AgentKind, CanonValue, Completeness, NormalizedTurn, RedactedBytes, Redactor,
        TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource, normalize_claude_transcript_until,
        normalize_codex_rollout_until, normalize_opencode_export_until, parse_canon_value,
        redact_turns_with_report, safe_turn_projection,
    },
};
use crate::{
    internal::{
        ai::{
            coverage_gate::{
                self, ImportIdentityCommit, ImportSessionCommit, LiveClaimCommitPlan,
                ReservedTurnClaim, merge_import_session_lifecycle,
            },
            history::{
                self, CheckpointCommitParams, CheckpointScope, HistoryManager, TracesInflightMarker,
            },
            hooks::{
                LifecycleEvent, LifecycleEventKind,
                lifecycle::{CanonicalEventContext, lifecycle_event_canonical_json_with_identity},
                runtime::build_ai_session_id,
            },
        },
        branch::TRACES_BRANCH,
    },
    utils::{client_storage::ClientStorage, util},
};

const IMPORT_IDENTITY_SCHEMA_VERSION: i64 = 1;
const IMPORT_LEASE_MS: i64 = 60_000;
const IMPORT_LIFECYCLE_EVENT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x7d, 0x9b, 0x4a, 0x51, 0x87, 0x44, 0x4d, 0x16, 0x9a, 0x6e, 0x05, 0x20, 0x26, 0x07, 0x15, 0x01,
]);
const AUTHORIZED_READ_HELPER_ARG: &str = "--libra-internal-authorized-read-helper";
const AUTHORIZED_READ_HELPER_CAP_ENV: &str = "LIBRA_INTERNAL_AUTHORIZED_READ_CAP";

#[derive(Debug, Error)]
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportRequest {
    pub agent_kind: AgentKind,
    pub provider_session_id: String,
    pub session_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub content_digest: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub session_state: String,
    pub stopped_at: Option<i64>,
    /// Verified current repository root. Raw transcript cwd values are never
    /// persisted (ADR-DR-15 compatibility exception).
    #[serde(with = "import_path_serde")]
    pub working_dir: PathBuf,
    pub repository_identity: String,
    /// Canonical repository/worktree/workspace owner. The bounded parsing
    /// helper cannot resolve this database-backed value, so the command path
    /// attaches it before the first lease/identity write.
    #[serde(default)]
    pub capture_scope: Option<CaptureScope>,
    pub source_fingerprint: String,
    /// Exact non-secret fingerprint of the pre-helper existing-session row.
    /// Foreground lease acquisition re-queries and compares this value without
    /// touching the row's potentially blocking filesystem path.
    pub existing_session_fingerprint: Option<String>,
    /// Aggregate typed-field redaction evidence for the allowlisted import
    /// projection; contains counts and rule ids, never raw matched bytes.
    pub redaction_report: serde_json::Value,
    pub turn_boundaries: BTreeMap<String, TurnBoundary>,
    pub turns: Vec<NormalizedTurn>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

#[derive(Debug, Error)]
#[error("historical import failed after durable progress: {message}")]
pub struct ImportProgressError {
    pub summary: ImportSummary,
    pub(crate) subagent_checkpoints_written: usize,
    import_identity_id: String,
    import_fence_token: i64,
    message: String,
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

#[derive(Default)]
struct SourceFacts {
    working_dirs: BTreeSet<PathBuf>,
    timestamps: Vec<i64>,
    provider_session_ids: BTreeSet<String>,
    terminal: bool,
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

fn import_test_pause_transcript_cwd_resolution() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_TRANSCRIPT_CWD_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_TRANSCRIPT_CWD_CONTINUE_FILE")
        .context("transcript-cwd pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready")
        .context("publish test-only transcript-cwd resolution pause")?;
    while !std::path::Path::new(&continue_path).exists() {
        // Models a canonicalize/storage lookup that cannot cooperate with a
        // Rust deadline; the outer preparation process is the kill boundary.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

fn verified_repo_root(
    facts: &SourceFacts,
    current_repo_root: &std::path::Path,
    current_storage_root: &std::path::Path,
) -> Result<PathBuf> {
    import_test_pause_transcript_cwd_resolution()?;
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
                Err(error) => {
                    return ImportSourceReadOutcome {
                        content: Err(error),
                        raw_bytes: 0,
                    };
                }
            };
            let program = match std::env::current_exe()
                .context("resolve Libra executable for bounded transcript reader")
            {
                Ok(program) => program,
                Err(error) => {
                    return ImportSourceReadOutcome {
                        content: Err(error),
                        raw_bytes: 0,
                    };
                }
            };
            let mut command = tokio::process::Command::new(program);
            command
                .arg(AUTHORIZED_READ_HELPER_ARG)
                .env(AUTHORIZED_READ_HELPER_CAP_ENV, read_cap.to_string())
                .stdin(Stdio::from(file))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let child = match command.spawn().context("start bounded transcript reader") {
                Ok(child) => child,
                Err(error) => {
                    return ImportSourceReadOutcome {
                        content: Err(error),
                        raw_bytes: 0,
                    };
                }
            };
            let output = match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child.wait_with_output(),
            )
            .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return ImportSourceReadOutcome {
                        content: Err(anyhow!(
                            "bounded transcript reader failed before completion: {error}"
                        )),
                        raw_bytes: 0,
                    };
                }
                Err(_) => {
                    return ImportSourceReadOutcome {
                        content: Err(ImportError::DeadlineExceeded.into()),
                        raw_bytes: 0,
                    };
                }
            };
            if !output.status.success() {
                return ImportSourceReadOutcome {
                    content: Err(anyhow!(
                        "bounded transcript reader exited unsuccessfully with status {}",
                        output.status
                    )),
                    raw_bytes: 0,
                };
            }
            if output.stdout.len() < 9 {
                return ImportSourceReadOutcome {
                    content: Err(anyhow!(
                        "bounded transcript reader returned a truncated frame"
                    )),
                    raw_bytes: 0,
                };
            }
            let status = output.stdout[0];
            let raw_bytes = match <[u8; 8]>::try_from(&output.stdout[1..9]) {
                Ok(bytes) => u64::from_le_bytes(bytes),
                Err(error) => {
                    return ImportSourceReadOutcome {
                        content: Err(anyhow!(
                            "bounded transcript reader returned an invalid byte count: {error}"
                        )),
                        raw_bytes: 0,
                    };
                }
            };
            let payload = &output.stdout[9..];
            let read = match status {
                0 if payload.len() as u64 == raw_bytes && raw_bytes <= read_cap => {
                    Ok(payload.to_vec())
                }
                0 => Err(anyhow!(
                    "bounded transcript reader returned an inconsistent success frame"
                )),
                1 if raw_bytes > read_cap && payload.is_empty() => Err(
                    super::observed_agents::transcript_source::TranscriptReadError::ExceedsCap {
                        cap: read_cap,
                    }
                    .into(),
                ),
                2 => Err(anyhow!(
                    "bounded transcript reader could not read the authorized descriptor: {}",
                    String::from_utf8_lossy(payload)
                )),
                other => Err(anyhow!(
                    "bounded transcript reader returned unknown status {other}"
                )),
            };
            if let Err(error) = ensure_before_deadline(deadline) {
                return ImportSourceReadOutcome {
                    content: Err(error),
                    raw_bytes,
                };
            }
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
    ensure_before_deadline(deadline)?;
    let AuthorizedImportContent {
        bytes,
        provisional_session_id,
    } = content;
    ensure_before_deadline(deadline)?;
    let facts = collect_facts(agent_kind, &bytes, Some(deadline))?;
    ensure_before_deadline(deadline)?;
    if facts.provider_session_ids.len() != 1
        || !facts
            .provider_session_ids
            .contains(selected_provider_session_id)
    {
        return Err(ImportError::SessionIdentityConflict.into());
    }
    let working_dir = verified_repo_root(&facts, current_repo_root, current_storage_root)?;
    let mut turns = match agent_kind {
        AgentKind::ClaudeCode => normalize_claude_transcript_until(&bytes, deadline),
        AgentKind::Codex => normalize_codex_rollout_until(&bytes, deadline),
        AgentKind::OpenCode => normalize_opencode_export_until(&bytes, deadline),
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
    let now = Utc::now().timestamp();
    let source_started_at = facts.timestamps.iter().copied().min().unwrap_or(now);
    let source_ended_at = facts
        .timestamps
        .iter()
        .copied()
        .max()
        .unwrap_or(source_started_at);
    let mut turn_boundaries = BTreeMap::new();
    let mut previous_ended_at: Option<i64> = None;
    for turn in &turns {
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
    let canonical_storage = current_storage_root
        .canonicalize()
        .context("canonicalize verified import repository storage")?;
    let repository_identity = hex::encode(Sha256::digest(
        canonical_storage.to_string_lossy().as_bytes(),
    ));
    let source_fingerprint = hex::encode(Sha256::digest(
        format!("{}\0{}\0{}", agent_kind.as_db_str(), source_kind, source_id).as_bytes(),
    ));
    let redaction_report = serde_json::json!({
        "pipeline": "typed_allowlist",
        "raw_persisted": false,
        "matches": typed_redaction_report.matches,
        "bytes_scanned": typed_redaction_report.bytes_scanned,
        "bytes_redacted": typed_redaction_report.bytes_redacted,
    });
    Ok(ImportRequest {
        agent_kind,
        provider_session_id: selected_provider_session_id.to_string(),
        session_id: provisional_session_id,
        source_kind: source_kind.to_string(),
        source_id: source_id.to_string(),
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
        turn_boundaries,
        turns,
    })
}

pub(crate) fn identity_id(request: &ImportRequest) -> String {
    let mut digest = Sha256::new();
    for value in [
        request.agent_kind.as_db_str(),
        request.provider_session_id.as_str(),
        request.source_kind.as_str(),
        request.source_id.as_str(),
        "1",
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("import-{}", hex::encode(digest.finalize()))
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

fn import_test_pause_existing_cwd_resolution() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_EXISTING_CWD_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_EXISTING_CWD_CONTINUE_FILE")
        .context("existing-cwd pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready")
        .context("publish test-only existing-cwd resolution pause")?;
    while !std::path::Path::new(&continue_path).exists() {
        // This deliberately models a filesystem syscall that cannot observe
        // the command deadline. The preparation helper is the kill boundary.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

/// Validate a pre-query existing-session row entirely inside the killable
/// preparation helper, including its persisted working-directory storage
/// identity. The parent retains only an exact row fingerprint for race-safe,
/// filesystem-free revalidation during lease acquisition.
pub fn validate_prepared_existing_session(
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
    import_test_pause_existing_cwd_resolution()?;
    let existing_cwd = PathBuf::from(&snapshot.working_dir)
        .canonicalize()
        .map_err(|_| ImportError::RepositoryConflict)?;
    let existing_storage = util::try_get_storage_path(Some(existing_cwd))
        .map_err(|_| ImportError::RepositoryConflict)?
        .canonicalize()
        .map_err(|_| ImportError::RepositoryConflict)?;
    let existing_repository_identity = hex::encode(Sha256::digest(
        existing_storage.to_string_lossy().as_bytes(),
    ));
    if existing_repository_identity != request.repository_identity {
        return Err(ImportError::RepositoryConflict.into());
    }
    let metadata: serde_json::Value = serde_json::from_str(&snapshot.metadata_json)
        .context("parse existing agent session ownership metadata")?;
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
    request.existing_session_fingerprint = Some(existing_session_snapshot_fingerprint(snapshot));
    Ok(())
}

async fn validate_existing_session_ownership<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
) -> Result<()> {
    let scope = capture_scope_for_request(conn, request).await?;
    let observed = load_existing_session_ownership_in_scope(
        conn,
        request.agent_kind,
        &request.provider_session_id,
        &scope,
    )
    .await?;
    let observed_fingerprint = observed.as_ref().map(existing_session_snapshot_fingerprint);
    if observed_fingerprint != request.existing_session_fingerprint {
        return Err(ImportError::RepositoryConflict.into());
    }
    Ok(())
}

async fn ensure_session(conn: &impl ConnectionTrait, request: &ImportRequest) -> Result<()> {
    let backend = conn.get_database_backend();
    let scope = capture_scope_for_request(conn, request).await?;
    scope
        .assert_provider_session_compatible(conn, &request.provider_session_id)
        .await?;
    scope.assert_workspace_fence_live(conn).await?;
    let existing = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT session_id, agent_kind, provider_session_id, working_dir, metadata_json
             FROM agent_session WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("query existing agent import session")?;
    if let Some(row) = existing {
        let snapshot = ExistingSessionOwnershipSnapshot {
            session_id: row.try_get_by("session_id")?,
            agent_kind: row.try_get_by("agent_kind")?,
            provider_session_id: row.try_get_by("provider_session_id")?,
            working_dir: row.try_get_by("working_dir")?,
            metadata_json: row.try_get_by("metadata_json")?,
        };
        if Some(existing_session_snapshot_fingerprint(&snapshot))
            != request.existing_session_fingerprint
            || snapshot.session_id != request.session_id
            || snapshot.agent_kind != request.agent_kind.as_db_str()
            || snapshot.provider_session_id != request.provider_session_id
        {
            return Err(ImportError::RepositoryConflict.into());
        }
        let metadata: serde_json::Value = serde_json::from_str(&snapshot.metadata_json)
            .context("parse existing agent session ownership metadata in lease transaction")?;
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
        // Validation only: lifecycle/working-dir/ownership mutation is
        // deferred to the first successful ref+catalog transaction.
        return Ok(());
    }
    if request.existing_session_fingerprint.is_some() {
        return Err(ImportError::RepositoryConflict.into());
    }
    let incarnation = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT next_session_sync_revision, source_namespace
             FROM agent_capture_incarnation
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("read restored agent capture replication incarnation")?;
    let (sync_revision, source_namespace) = if let Some(row) = incarnation {
        (
            row.try_get_by::<i64, _>("next_session_sync_revision")?,
            Some(row.try_get_by::<String, _>("source_namespace")?),
        )
    } else {
        (1, None)
    };
    let mut metadata = serde_json::json!({
        "import_provisional": true,
        "source_kind": request.source_kind,
        "source_id": request.source_id,
        "repository_identity": request.repository_identity,
        "source_fingerprint": request.source_fingerprint,
    });
    if let Some(source_namespace) = source_namespace {
        metadata["capture_incarnation"] = serde_json::Value::String(source_namespace);
    }
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            stopped_at, schema_version, sync_revision,
            repo_id, worktree_id, workspace_id, workspace_fence, scope_state
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, 'scoped')",
        [
            request.session_id.clone().into(),
            request.agent_kind.as_db_str().into(),
            request.provider_session_id.clone().into(),
            "pending".into(),
            request.working_dir.to_string_lossy().into_owned().into(),
            metadata.to_string().into(),
            serde_json::json!({"import": request.redaction_report})
                .to_string()
                .into(),
            request.started_at.into(),
            request.started_at.into(),
            Value::from(None::<i64>),
            sync_revision.into(),
            scope.repo_id.clone().into(),
            scope.worktree_id.clone().into(),
            scope.workspace_id.clone().into(),
            scope.workspace_fence.into(),
        ],
    ))
    .await
    .context("create agent session for historical import")?;
    Ok(())
}

async fn assert_import_identity_scope<C: ConnectionTrait>(
    conn: &C,
    request: &ImportRequest,
    scope: &CaptureScope,
) -> Result<()> {
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
                IMPORT_IDENTITY_SCHEMA_VERSION.into(),
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
    deadline: Instant,
) -> Result<ImportLease> {
    let identity_id = identity_id(request);
    let scope = capture_scope_for_request(conn, request).await?;
    let lease_expires_at = now_ms
        .checked_add(IMPORT_LEASE_MS)
        .context("import lease timestamp overflow")?;
    let txn = conn.begin().await.context("begin import identity lease")?;
    // Acquire SQLite's writer slot before checking the tombstone and the
    // prepared session fingerprint. Otherwise erasure can commit between a
    // standalone ownership read and this transaction, turning a known erased
    // identity into a misleading repository-conflict result.
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        // `working_dir` is deliberately outside the compatibility tombstone
        // trigger's UPDATE OF list. Updating a guarded lifecycle column here
        // would abort before the contextual tombstone check can return
        // ImportError::Erased when erase has committed its first phase.
        "UPDATE agent_session SET working_dir = working_dir
         WHERE agent_kind = ? AND provider_session_id = ?
           AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
           AND workspace_id IS ? AND workspace_fence IS ?",
        [
            request.agent_kind.as_db_str().into(),
            request.provider_session_id.clone().into(),
            scope.repo_id.clone().into(),
            scope.worktree_id.clone().into(),
            scope.workspace_id.clone().into(),
            scope.workspace_fence.into(),
        ],
    ))
    .await
    .context("serialize import identity acquisition with session erasure")?;
    let tombstone = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check import tombstone")?;
    if tombstone.is_some() {
        txn.rollback().await.ok();
        return Err(ImportError::Erased.into());
    }
    validate_existing_session_ownership(&txn, request).await?;
    // Keep the write barrier and session insert under the same SQLite writer
    // transaction. Otherwise erase can commit a tombstone between the check
    // and a standalone session insert, leaving a locally erased session
    // resurrected even though identity acquisition fails closed.
    ensure_session(&txn, request).await?;
    assert_import_identity_scope(&txn, request, &scope).await?;
    let inserted = txn
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
                IMPORT_IDENTITY_SCHEMA_VERSION.into(),
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
        .context("insert import identity lease")?;
    let fence_token = if inserted.rows_affected() == 1 {
        1
    } else {
        let row = txn
            .query_one_raw(Statement::from_sql_and_values(
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
                    IMPORT_IDENTITY_SCHEMA_VERSION.into(),
                    scope.repo_id.clone().into(),
                    scope.worktree_id.clone().into(),
                    scope.workspace_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .context("read import identity lease")?
            .context("import identity disappeared during lease acquisition")?;
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
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_coverage_claim
                 SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
                     attempt_checkpoint_id = NULL,
                     fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
                 WHERE session_id = ? AND state = 'reserved_import' AND owner = ?",
                [
                    now_ms.into(),
                    request.session_id.clone().into(),
                    stale_owner.into(),
                ],
            ))
            .await
            .context("abandon crashed import owner's stale coverage reservations")?;
        }
        if let Some(stale_checkpoint_id) = stale_attempt_checkpoint_id {
            history::retire_stale_traces_inflight_marker(
                &txn,
                &request.session_id,
                &stale_checkpoint_id,
            )
            .await
            .context("retire fenced crashed import attempt marker")?;
        }
        next_fence
    };
    if let Err(error) = ensure_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error);
    }
    txn.commit().await.context("commit import identity lease")?;
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
    deadline: Instant,
) -> Result<String> {
    ensure_before_deadline(deadline)?;
    let scope = capture_scope_for_request(conn, request).await?;
    let txn = conn.begin().await.context("begin import attempt binding")?;
    let lease_expires_at = now_ms
        .checked_add(IMPORT_LEASE_MS)
        .context("import lease timestamp overflow during attempt binding")?;
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
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE agent_coverage_claim SET lease_expires_at = ?, updated_at = ?
         WHERE session_id = ? AND state = 'reserved_import' AND owner = ?",
        [
            lease_expires_at.into(),
            now_ms.into(),
            request.session_id.clone().into(),
            lease.owner.clone().into(),
        ],
    ))
    .await
    .context("renew remaining import coverage reservations")?;
    let coverage = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_coverage_claim SET attempt_checkpoint_id = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ? AND state = 'reserved_import'
               AND owner = ? AND fence_token = ?",
            [
                checkpoint_id.into(),
                now_ms.into(),
                request.session_id.clone().into(),
                claim.logical_turn_key.clone().into(),
                super::observed_agents::COVERAGE_SCHEMA_VERSION.into(),
                lease.owner.clone().into(),
                claim.fence_token.into(),
            ],
        ))
        .await
        .context("bind import coverage attempt")?;
    if coverage.rows_affected() != 1 {
        txn.rollback().await.ok();
        bail!("import coverage claim was fenced out before object construction");
    }
    // The attempt marker and the tombstone/fence checks above share one
    // SQLite writer transaction. An erase therefore either observes this
    // marker before reporting success or wins first and prevents all object
    // construction for the stale importer.
    let marker = TracesInflightMarker::new(&request.session_id, checkpoint_id, now_ms);
    let marker_generation = marker
        .generation
        .clone()
        .context("new import marker has no writer generation")?;
    history::write_traces_inflight_marker(&txn, &marker)
        .await
        .context("write import traces in-flight marker in attempt transaction")?;
    if let Err(error) = ensure_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error);
    }
    txn.commit()
        .await
        .context("commit import attempt binding")?;
    Ok(marker_generation)
}

async fn finalize_noop_identity(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    state: &str,
    last_error_code: Option<&str>,
    now_ms: i64,
    deadline: Instant,
) -> Result<()> {
    let scope = capture_scope_for_request(conn, request).await?;
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .context("begin import identity finalization")?;
    let tombstone = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check import tombstone during finalization")?;
    if tombstone.is_some() {
        txn.rollback().await.ok();
        return Err(ImportError::Erased.into());
    }
    let committed_digest: Value = if state == "committed" {
        Some(request.content_digest.clone()).into()
    } else {
        None::<String>.into()
    };
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
        let session = ImportSessionCommit {
            working_dir: request.working_dir.to_string_lossy().into_owned(),
            state: request.session_state.clone(),
            started_at: request.started_at,
            last_event_at: request.ended_at,
            stopped_at: request.stopped_at,
            ownership_metadata_json: serde_json::json!({
                "imported": true,
                "import_provisional": false,
                "source_kind": request.source_kind,
                "source_id": request.source_id,
                "repository_identity": request.repository_identity,
                "source_fingerprint": request.source_fingerprint,
            })
            .to_string(),
            redaction_report_json: serde_json::json!({
                "import": request.redaction_report,
            })
            .to_string(),
        };
        merge_import_session_lifecycle(&txn, &request.session_id, &session).await?;
    }
    if let Err(error) = ensure_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error);
    }
    txn.commit()
        .await
        .context("commit import identity finalization")?;
    Ok(())
}

async fn abandon_import_attempt(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    marker_fence: Option<(&str, &str)>,
    identity_state: &str,
    last_error_code: &str,
    now_ms: i64,
) -> Result<()> {
    let scope = capture_scope_for_request(conn, request).await?;
    let txn = conn
        .begin()
        .await
        .context("begin expired import attempt cleanup")?;
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE agent_coverage_claim
         SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
             attempt_checkpoint_id = NULL,
             fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
         WHERE session_id = ?
           AND coverage_schema_version = ? AND state = 'reserved_import'
           AND owner = ?",
        [
            now_ms.into(),
            request.session_id.clone().into(),
            super::observed_agents::COVERAGE_SCHEMA_VERSION.into(),
            lease.owner.clone().into(),
        ],
    ))
    .await
    .context("abandon expired import coverage reservation")?;
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
                identity_state.into(),
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
        .context("release expired import identity lease")?
        .rows_affected()
        == 1;
    if released_identity && let Some((checkpoint_id, marker_generation)) = marker_fence {
        // A rejected append may have converted this marker into a durable
        // cleanup job. Never erase that ownership record here.
        history::clear_non_cleanup_traces_inflight_marker(
            &txn,
            &request.session_id,
            checkpoint_id,
            marker_generation,
        )
        .await
        .context("clear uncommitted import attempt marker")?;
    }
    if released_identity {
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "DELETE FROM agent_session
             WHERE session_id = ?
               AND COALESCE(json_extract(metadata_json, '$.import_provisional'), 0) = 1
               AND NOT EXISTS (
                 SELECT 1 FROM agent_checkpoint c
                 WHERE c.session_id = agent_session.session_id
               )
               AND NOT EXISTS (
                 SELECT 1 FROM agent_import_identity i
                 WHERE i.agent_kind = agent_session.agent_kind
                   AND i.provider_session_id = agent_session.provider_session_id
                   AND i.owner IS NOT NULL
               )",
            [request.session_id.clone().into()],
        ))
        .await
        .context("remove zero-progress provisional import session")?;
    }
    txn.commit()
        .await
        .context("commit expired import attempt cleanup")?;
    Ok(())
}

async fn abandon_import_attempt_after_error(
    conn: &DatabaseConnection,
    request: &ImportRequest,
    lease: &ImportLease,
    marker_fence: Option<(&str, &str)>,
    identity_state: &str,
    original: anyhow::Error,
) -> anyhow::Error {
    match abandon_import_attempt(
        conn,
        request,
        lease,
        marker_fence,
        identity_state,
        "LBR-AGENT-018",
        Utc::now().timestamp_millis(),
    )
    .await
    {
        Ok(()) => original,
        Err(cleanup_error) => {
            tracing::error!(
                session_id = %request.session_id,
                identity_id = %lease.identity_id,
                error = %format!("{cleanup_error:#}"),
                "failed to abandon an unsuccessful historical import attempt"
            );
            original.context(format!(
                "the import also failed to release its recovery ownership: {cleanup_error:#}; \
                 run `libra agent doctor --repair` before retrying or erasing this session"
            ))
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

fn import_test_failpoint(name: &str) -> Result<()> {
    if cfg!(debug_assertions) && std::env::var("LIBRA_TEST_IMPORT_FAILPOINT").as_deref() == Ok(name)
    {
        bail!("test-injected import failure at {name}");
    }
    Ok(())
}

fn preserving_crash_failpoint_active() -> bool {
    cfg!(debug_assertions)
        && matches!(
            std::env::var("LIBRA_TEST_IMPORT_FAILPOINT").as_deref(),
            Ok("after_bind")
        )
}

fn import_test_pause_after_bind(deadline: Instant) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_AFTER_BIND_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_AFTER_BIND_CONTINUE_FILE")
        .context("after-bind pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready").context("publish test-only after-bind import pause")?;
    while !std::path::Path::new(&continue_path).exists() {
        ensure_before_deadline(deadline)?;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

fn import_test_pause_before_bind(deadline: Instant) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_BEFORE_BIND_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_BEFORE_BIND_CONTINUE_FILE")
        .context("before-bind pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready").context("publish test-only before-bind import pause")?;
    while !std::path::Path::new(&continue_path).exists() {
        ensure_before_deadline(deadline)?;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

async fn current_parent_commit(conn: &DatabaseConnection) -> Result<Option<String>> {
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
}

async fn committed_source_ordinals(
    conn: &DatabaseConnection,
    request: &ImportRequest,
) -> Result<BTreeSet<usize>> {
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
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
        .context("load committed source ordinals for import cursor")?;
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

async fn checkpoint_is_cataloged(conn: &DatabaseConnection, checkpoint_id: &str) -> Result<bool> {
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_checkpoint WHERE checkpoint_id = ?",
            [checkpoint_id.into()],
        ))
        .await
        .context("probe imported checkpoint after ambiguous append completion")?
        .is_some())
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
    storage_root: &std::path::Path,
    request: &ImportRequest,
    discovery: &super::subagent_content::SubagentDiscovery,
    deadline: Instant,
) -> Result<super::subagent_content::SubagentCaptureSummary> {
    if request.agent_kind != AgentKind::ClaudeCode {
        return Ok(super::subagent_content::SubagentCaptureSummary::default());
    }
    if let Some(warning) = discovery.warning.as_deref() {
        tracing::warn!(
            session_id = %request.session_id,
            warning,
            "historical import subagent discovery is unavailable; parent import remains valid"
        );
    }
    let summary = super::subagent_content::capture_discovered_subagent_contents(
        conn,
        storage_root,
        &request.session_id,
        &discovery.sources,
        "import",
        Some(deadline),
    )
    .await?;
    tracing::info!(
        session_id = %request.session_id,
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
    deadline: Instant,
) -> Result<ImportSummary> {
    let discovery = if request.agent_kind == AgentKind::ClaudeCode {
        let discovery_deadline =
            super::subagent_content::discovery_deadline_preserving_parent(deadline)?;
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
    deadline: Instant,
    subagent_discovery: super::subagent_content::SubagentDiscovery,
) -> Result<DetailedImportSummary> {
    ensure_before_deadline(deadline)?;
    let owner = format!("import:{}:{}", std::process::id(), uuid::Uuid::new_v4());
    let started_ms = Utc::now().timestamp_millis();
    let lease = acquire_identity(conn, &request, &owner, started_ms, deadline).await?;
    if let Err(error) = ensure_before_deadline(deadline) {
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error).await;
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
            )
            .await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_before_deadline(deadline) {
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error).await;
        return Err(error);
    }
    // A discovery warning (for example, secure child discovery being
    // unavailable on this platform) does not prove that child evidence exists.
    // Only observed incomplete sources or claim conflicts make the import
    // partial; the parent transcript remains independently valid.
    let partial = historical_import_is_partial(
        outcome.skipped_inflight,
        outcome.conflicted,
        &subagent_discovery,
    );
    let defer_identity_for_subagents = !subagent_discovery.sources.is_empty();
    if outcome.reserved.is_empty() {
        let identity_state = if partial { "partial" } else { "committed" };
        let capture = capture_imported_subagent_content(
            conn,
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
                let error = abandon_import_attempt_after_error(
                    conn,
                    &request,
                    &lease,
                    None,
                    "partial",
                    error
                        .context("capture subagent content for already-covered historical session"),
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
                    message: format!("subagent content attribution failed: {error:#}"),
                }
                .into());
            }
        };
        if capture_summary.partial_sources > 0 && !partial {
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                "partial",
                anyhow!("subagent partial-source accounting changed during import finalization"),
            )
            .await;
            return Err(error);
        }
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

    let objects_dir = storage_root.join("objects");
    let manager = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init_local_existing(objects_dir)),
        storage_root.to_path_buf(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let parent_commit = match current_parent_commit(conn).await {
        Ok(parent) => parent,
        Err(error) => {
            let error =
                abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error)
                    .await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_before_deadline(deadline) {
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error).await;
        return Err(error);
    }
    let mut committed_ordinals = match committed_source_ordinals(conn, &request).await {
        Ok(ordinals) => ordinals,
        Err(error) => {
            let error =
                abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error)
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
    if let Err(error) = import_test_pause_before_bind(deadline) {
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "failed", error).await;
        return Err(error);
    }
    for (claim_index, claim) in reserved.iter().enumerate() {
        let mut bound_checkpoint_id = None;
        let mut bound_marker_generation = None;
        let turn_result: Result<()> = async {
            ensure_before_deadline(deadline)?;
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
            let marker_generation = bind_attempt(
                conn,
                &request,
                &lease,
                claim,
                &checkpoint_id,
                now_ms,
                deadline,
            )
            .await?;
            bound_marker_generation = Some(marker_generation.clone());
            import_test_pause_after_bind(deadline)?;
            import_test_failpoint("after_bind")?;
            ensure_before_deadline(deadline)?;
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
            // Only imports with actual child sources defer identity completion.
            // The established no-child path retains its atomic parent
            // checkpoint+identity commit and post-commit deadline semantics.
            let final_turn = claim_index + 1 == reserved.len()
                && !partial
                && !defer_identity_for_subagents;
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
                import_session: Some(ImportSessionCommit {
                    working_dir: request.working_dir.to_string_lossy().into_owned(),
                    state: request.session_state.clone(),
                    started_at: request.started_at,
                    last_event_at: request.ended_at,
                    stopped_at: request.stopped_at,
                    ownership_metadata_json: serde_json::json!({
                        "imported": true,
                        "import_provisional": false,
                        "source_kind": request.source_kind,
                        "source_id": request.source_id,
                        "repository_identity": request.repository_identity,
                        "source_fingerprint": request.source_fingerprint,
                    })
                    .to_string(),
                    redaction_report_json: serde_json::json!({
                        "import": request.redaction_report,
                    })
                    .to_string(),
                }),
                import_identity: Some(ImportIdentityCommit {
                    identity_id: lease.identity_id.clone(),
                    observed_digest: request.content_digest.clone(),
                    owner: lease.owner.clone(),
                    fence_token: lease.fence_token,
                    next_ordinal,
                    final_turn,
                }),
            };
            let append = manager
                .append_checkpoint_commit(CheckpointCommitParams {
                    checkpoint_id: &checkpoint_id,
                    session_id: &request.session_id,
                    marker_generation: &marker_generation,
                    agent_kind: request.agent_kind.as_db_str(),
                    parent_commit: parent_commit.as_deref(),
                    scope: CheckpointScope::Committed,
                    tool_use_id: None,
                    metadata_json: &metadata,
                    transcript_redacted: &transcript,
                    lifecycle_events_jsonl: &lifecycle,
                    redaction_report_json: &report,
                    txn_extra: Some(&plan),
                    deadline: Some(deadline),
                })
                .await;
            let append_committed = append.is_ok()
                || checkpoint_is_cataloged(conn, &checkpoint_id).await?;
            if append_committed
                && let Some(marker_generation) = bound_marker_generation.as_deref()
                && let Err(marker_error) = history::clear_non_cleanup_traces_inflight_marker(
                    conn,
                    &request.session_id,
                    &checkpoint_id,
                    marker_generation,
                )
                .await
            {
                tracing::warn!(
                    session_id = %request.session_id,
                    checkpoint_id = %checkpoint_id,
                    error = %format!("{marker_error:#}"),
                    "failed to clear successful import traces in-flight marker; it will expire by TTL"
                );
            }
            if let Err(error) = append
                && !append_committed
            {
                return Err(error.context("append imported turn checkpoint"));
            }
            written += 1;
            import_test_failpoint("after_catalog_commit")?;
            if claim_index + 1 < reserved.len() {
                ensure_before_deadline(deadline)?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = turn_result {
            let terminal_state = if written == 0 { "failed" } else { "partial" };
            let error = if preserving_crash_failpoint_active() {
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
                message: format!("{error:#}"),
            }
            .into());
        }
    }
    let capture = capture_imported_subagent_content(
        conn,
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
            let error = abandon_import_attempt_after_error(
                conn,
                &request,
                &lease,
                None,
                "partial",
                error.context("capture imported subagent content"),
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
                message: format!("subagent content attribution failed: {error:#}"),
            }
            .into());
        }
    };
    if capture_summary.partial_sources > 0 && !partial {
        let error =
            anyhow!("subagent partial-source accounting changed during import finalization");
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "partial", error)
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
            message: format!("{error:#}"),
        }
        .into());
    }
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
        let error =
            abandon_import_attempt_after_error(conn, &request, &lease, None, "partial", error)
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
            message: format!("{error:#}"),
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
            Some((checkpoint_id, &stale_generation)),
            "failed",
            "LBR-AGENT-018",
            2,
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
        )
        .await;
        let surfaced = format!("{surfaced:#}");
        assert!(surfaced.contains("primary import failure"), "{surfaced}");
        assert!(surfaced.contains("forced abandon failure"), "{surfaced}");
        assert!(surfaced.contains("agent doctor --repair"), "{surfaced}");
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
}
