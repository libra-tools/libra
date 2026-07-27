//! `libra agent import` — consented historical transcript backfill (M4).

use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Args};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::{
    internal::{
        ai::{
            agent_import::{
                DetailedImportSummary, ExistingSessionOwnershipSnapshot, ImportError,
                ImportPreparationContext, ImportProgressError, ImportRequest, ImportSummary,
                identity_id as import_identity_id, import_prepared_with_subagent_discovery,
                load_existing_session_ownership, prepare_import_request, read_import_source,
                restore_tombstone, session_is_tombstoned, validate_prepared_existing_session,
            },
            observed_agents::{
                AgentKind, AgentSessionCtx, TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource,
                agent_for, claude_session_dir, claude_session_id_is_safe_path_component,
                compliance::{MAX_TRANSCRIPT_READ_BYTES_KEY, max_transcript_read_bytes_setting},
                find_codex_rollout, open_provider_directory_for_discovery,
                opencode_export::{
                    ExportLimits, authorized_sandboxed_export, trusted_opencode_binary,
                },
                pinned_provider_directory_path, resolve_import_transcript_source_until,
                resolve_session_file,
            },
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

pub const AGENT_IMPORT_EXAMPLES: &str = "\
EXAMPLES:
    libra agent import --session <id> --agent claude-code --yes
    libra agent import --session <id> --agent codex --yes
    libra agent import --session <id> --agent opencode --yes
    libra agent import --path ~/.claude/projects/<project>/<id>.jsonl --agent claude-code --yes
    libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes
    libra agent import --all --agent claude-code --limit 20 --yes";

const DEFAULT_IMPORT_LIMIT: usize = 20;
const MAX_IMPORT_LIMIT: usize = 100;
const MAX_BATCH_RAW_BYTES: u64 = 64 * 1024 * 1024;
const IMPORT_TOTAL_DEADLINE: Duration = Duration::from_secs(120);
pub const IMPORT_DISCOVERY_HELPER_ARG: &str = "--libra-internal-agent-import-discovery-helper";
/// Compact base64 path frames keep a full public 100-result page (including
/// near-`PATH_MAX` Unix paths) below this deliberately derived 2 MiB ceiling.
pub const IMPORT_DISCOVERY_HELPER_FRAME_CAP: u64 = 2 * 1024 * 1024;
pub const IMPORT_PREPARATION_HELPER_ARG: &str = "--libra-internal-agent-import-preparation-helper";
pub const IMPORT_PREPARATION_HELPER_INPUT_CAP: u64 = 2 * 1024 * 1024;
pub const IMPORT_PREPARATION_HELPER_OUTPUT_CAP: u64 = 128 * 1024 * 1024;
pub const IMPORT_INDEX_REPAIR_HELPER_ARG: &str =
    "--libra-internal-agent-import-index-repair-helper";
pub const IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP: u64 = 64 * 1024;
const IMPORT_INDEX_REPAIR_MARKER_KEY: &str = "object-index-v1";
/// Longer than the command's absolute deadline so a healthy importer cannot
/// be preempted while it is finishing a final SQLite commit or index drain.
/// Normal failures explicitly release the lease into `repair_pending`, so
/// only a process crash requires waiting for this TTL.
const IMPORT_INDEX_BARRIER_LEASE_MS: i64 = 180_000;

fn import_total_deadline() -> Duration {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("LIBRA_TEST_IMPORT_DEADLINE_MS")
        && let Ok(parsed) = value.parse::<u64>()
        && parsed > 0
    {
        return Duration::from_millis(parsed).min(IMPORT_TOTAL_DEADLINE);
    }
    IMPORT_TOTAL_DEADLINE
}

fn ensure_before_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(ImportError::DeadlineExceeded.into());
    }
    Ok(())
}

fn import_test_pause_after_source_open(deadline: Instant) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_SOURCE_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_SOURCE_CONTINUE_FILE")
        .context("source-open pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready").context("publish test-only source-open import pause")?;
    while !Path::new(&continue_path).exists() {
        ensure_before_deadline(deadline)?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn import_test_pause_after_index_barrier(deadline: Instant) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_INDEX_BARRIER_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_INDEX_BARRIER_CONTINUE_FILE")
        .context("index-barrier pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready")
        .context("publish test-only import index-barrier pause")?;
    while !Path::new(&continue_path).exists() {
        ensure_before_deadline(deadline)?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn batch_raw_byte_cap() -> u64 {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("LIBRA_TEST_IMPORT_BATCH_CAP_BYTES")
        && let Ok(parsed) = value.parse::<u64>()
        && parsed > 0
    {
        return parsed.min(MAX_BATCH_RAW_BYTES);
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
    group(ArgGroup::new("selector").required(true).multiple(false).args(["session", "path", "since", "all"]))
)]
pub struct ImportArgs {
    /// Import one provider session id.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Import one transcript below the selected provider root.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Discover sessions modified since this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    /// Discover all bounded local Claude/Codex session sources.
    #[arg(long)]
    pub all: bool,
    /// Provider filter (`claude-code`, `codex`, or `opencode`).
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
    #[arg(long, requires = "yes")]
    pub restore_erased: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    kind: AgentKind,
    provider_session_id: String,
    path: Option<PathBuf>,
}

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
#[serde(tag = "status", rename_all = "snake_case")]
enum DiscoveryHelperResponse {
    Ok {
        candidates: Vec<DiscoveryCandidateWire>,
        next_cursor: Option<usize>,
    },
    Error {
        stable_code: String,
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct PreparationHelperRequest {
    candidate: DiscoveryCandidateWire,
    repo_root: WirePath,
    storage_root: WirePath,
    read_cap: u64,
    remaining_ms: u64,
    existing_session: Option<ExistingSessionOwnershipSnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PreparationHelperResponse {
    Ok {
        request: Box<ImportRequest>,
        raw_bytes: u64,
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum IndexRepairHelperResponse {
    Ok { repaired_rows: usize },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportIndexBarrierMarker {
    schema_version: u32,
    owner: String,
    generation: String,
    identity_id: String,
    agent_kind: String,
    provider_session_id: String,
    source_kind: String,
    source_id: String,
    state: String,
    lease_expires_at: i64,
    created_at: i64,
    #[serde(default)]
    fence_token: Option<i64>,
}

#[derive(Debug, Clone)]
struct ImportIndexBarrier {
    session_id: String,
    marker: ImportIndexBarrierMarker,
}

#[derive(Debug, Clone)]
struct ImportIdentityFence {
    identity_id: String,
    fence_token: i64,
}

struct PreparedCandidateOutcome {
    request: Result<ImportRequest>,
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
}

#[derive(Debug, Serialize)]
struct BatchFailure {
    status: &'static str,
    agent_kind: String,
    session_id: String,
    error_code: StableErrorCode,
}

fn importable_kind(slug: &str) -> CliResult<AgentKind> {
    match AgentKind::from_cli_slug(slug) {
        Some(kind @ (AgentKind::ClaudeCode | AgentKind::Codex | AgentKind::OpenCode)) => Ok(kind),
        _ => Err(CliError::command_usage(format!(
            "agent import supports claude-code, codex, or opencode; got '{slug}'"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)),
    }
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
        AgentKind::Codex | AgentKind::OpenCode => codex_session_id_is_safe_path_component(value),
        AgentKind::ClaudeCode => claude_session_id_is_safe_path_component(value),
        _ => false,
    }
}

fn validate_session_id_for_kind(value: &str, kind: AgentKind) -> CliResult<()> {
    if session_id_is_valid_for_kind(value, kind) {
        Ok(())
    } else {
        let expected = match kind {
            AgentKind::Codex | AgentKind::OpenCode => {
                "alphanumeric/dash/underscore, at most 64 characters"
            }
            AgentKind::ClaudeCode => "alphanumeric/dot/dash/underscore, at most 128 characters",
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

fn claude_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    validate_session_id_for_kind(&stem, AgentKind::ClaudeCode)
        .ok()
        .map(|()| stem)
}

fn path_candidate(path: PathBuf, kind: AgentKind) -> CliResult<Candidate> {
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
        return Err(std::io::Error::last_os_error()).context("inspect provider discovery entry");
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
        return Err(std::io::Error::last_os_error())
            .context("open provider discovery entry without following links");
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
    let path = pinned_provider_directory_path(directory);
    let read = std::fs::read_dir(&path)
        .with_context(|| format!("read pinned {label} directory {}", path.display()))?;
    let mut names = Vec::new();
    for entry in read {
        ensure_before_deadline(deadline)?;
        *scanned = scanned
            .checked_add(1)
            .context("provider discovery entry counter overflow")?;
        if *scanned > 20_000 {
            anyhow::bail!("{label} discovery exceeded its 20000-entry safety bound");
        }
        names.push(
            entry
                .context("read pinned provider directory entry")?
                .file_name(),
        );
    }
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
    let pinned_path = pinned_provider_directory_path(&directory);
    #[cfg(not(unix))]
    let pinned_path = dir.clone();
    let entries = match std::fs::read_dir(&pinned_path) {
        Ok(entries) => entries,
        Err(error) => return Err(error).context("read pinned Claude session directory"),
    };
    let mut candidates = Vec::new();
    for (index, entry) in entries.enumerate() {
        ensure_before_deadline(deadline)?;
        if index >= 20_000 {
            anyhow::bail!("Claude discovery exceeded its 20000-entry safety bound");
        }
        let entry = entry.context("read Claude session directory entry")?;
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
    std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .map(|home| home.join(".codex").join("sessions"))
}

fn discover_codex(since: Option<i64>, deadline: Instant) -> Result<Vec<Candidate>> {
    ensure_before_deadline(deadline)?;
    let Some(root) = codex_sessions_root() else {
        return Ok(Vec::new());
    };
    let adapter = agent_for(AgentKind::Codex);
    let Some(directory) = open_provider_directory_for_discovery(adapter, &root)? else {
        return Ok(Vec::new());
    };
    #[cfg(unix)]
    return discover_codex_unix(&root, &directory, since, deadline);
    #[cfg(not(unix))]
    {
        let pinned_root = root.clone();
        let mut scanned = 0usize;
        let mut candidates = Vec::new();
        for year in read_codex_discovery_directory(&pinned_root, deadline, &mut scanned)? {
            let Some(year_name) = year.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if year_name.len() != 4 || !year_name.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Some(year_number) = year_name.parse::<i32>().ok().filter(|year| *year > 0) else {
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
                for day in read_codex_discovery_directory(&month.path(), deadline, &mut scanned)? {
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
                    for file in read_codex_discovery_directory(&day.path(), deadline, &mut scanned)?
                    {
                        let file_type = file
                            .file_type()
                            .context("inspect Codex rollout discovery entry")?;
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
                        if pinned_path.extension().and_then(|value| value.to_str()) != Some("jsonl")
                        {
                            continue;
                        }
                        if let Some(boundary) = since
                            && metadata_modified_at(
                                &file.metadata().context("inspect pinned Codex rollout")?,
                            )? < boundary
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
}

#[cfg(unix)]
fn discover_codex_unix(
    logical_root: &Path,
    directory: &std::fs::File,
    since: Option<i64>,
    deadline: Instant,
) -> Result<Vec<Candidate>> {
    let mut scanned = 0usize;
    let mut candidates = Vec::new();
    for year_name in read_discovery_names(directory, "Codex rollout", deadline, &mut scanned)? {
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
        for month_name in read_discovery_names(&year, "Codex rollout", deadline, &mut scanned)? {
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
            for day_name in read_discovery_names(&month, "Codex rollout", deadline, &mut scanned)? {
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
                for file_name in
                    read_discovery_names(&day, "Codex rollout", deadline, &mut scanned)?
                {
                    let Some(file) =
                        open_discovery_entry_at(&day, &file_name, DiscoveryEntryKind::File)?
                    else {
                        continue;
                    };
                    let logical_path = logical_root
                        .join(&year_name)
                        .join(&month_name)
                        .join(&day_name)
                        .join(&file_name);
                    if logical_path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if let Some(boundary) = since
                        && metadata_modified_at(
                            &file.metadata().context("inspect opened Codex rollout")?,
                        )? < boundary
                    {
                        continue;
                    }
                    let Some(provider_session_id) = codex_session_id_from_filename(&logical_path)
                    else {
                        continue;
                    };
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
            anyhow::bail!("Codex discovery exceeded its 20000-entry safety bound");
        }
        entries.push(
            entry
                .with_context(|| format!("read Codex rollout entry in {}", directory.display()))?,
        );
        ensure_before_deadline(deadline)?;
    }
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

fn discovery_error(error: anyhow::Error, message: &'static str) -> CliError {
    if matches!(
        error.downcast_ref::<ImportError>(),
        Some(ImportError::DeadlineExceeded)
    ) {
        CliError::fatal("agent import discovery exceeded its total execution deadline")
            .with_stable_code(StableErrorCode::AgentImportPartialBatch)
    } else {
        CliError::fatal(message)
            .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing)
    }
}

fn discover(
    args: &ImportArgs,
    repo_root: &Path,
    deadline: Instant,
) -> CliResult<(Vec<Candidate>, Option<usize>)> {
    ensure_before_deadline(deadline).map_err(|error| {
        discovery_error(
            error,
            "agent import discovery exceeded its execution deadline",
        )
    })?;
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
        return Ok((vec![path_candidate(path, kind)?], None));
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
        if filter == Some(AgentKind::OpenCode) {
            return Ok((
                vec![Candidate {
                    kind: AgentKind::OpenCode,
                    provider_session_id: session_id.to_string(),
                    path: None,
                }],
                None,
            ));
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
                        discovery_error(
                            error,
                            "Claude session discovery failed within its configured provider root",
                        )
                    })?;
                    ensure_before_deadline(deadline).map_err(|error| {
                        discovery_error(
                            error,
                            "Claude session discovery exceeded its execution deadline",
                        )
                    })?;
                    found
                }
                AgentKind::Codex => {
                    let found = find_codex_rollout(session_id).map_err(|error| {
                        discovery_error(
                            error,
                            "Codex session discovery failed within its configured provider root",
                        )
                    })?;
                    ensure_before_deadline(deadline).map_err(|error| {
                        discovery_error(
                            error,
                            "Codex session discovery exceeded its execution deadline",
                        )
                    })?;
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
            return Err(CliError::command_usage(
                "the session id matches multiple providers; add --agent",
            )
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        if candidates.is_empty() {
            return Err(CliError::fatal(
                "no authorized local transcript matched the session id; use --agent opencode for an export-only OpenCode session",
            )
            .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
        return Ok((candidates, None));
    }

    if filter == Some(AgentKind::OpenCode) {
        return Err(CliError::command_usage(
            "OpenCode batch discovery is unavailable; select a session explicitly with --session",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let since = parse_since(args.since.as_deref())?;
    let mut candidates = Vec::new();
    if filter.is_none() || filter == Some(AgentKind::ClaudeCode) {
        candidates.extend(
            discover_claude(repo_root, since, deadline).map_err(|error| {
                discovery_error(
                    error,
                    "Claude session discovery failed within its configured provider root",
                )
            })?,
        );
    }
    if filter.is_none() || filter == Some(AgentKind::Codex) {
        candidates.extend(discover_codex(since, deadline).map_err(|error| {
            discovery_error(
                error,
                "Codex session discovery failed within its configured provider root",
            )
        })?);
    }
    ensure_before_deadline(deadline).map_err(|error| {
        discovery_error(
            error,
            "agent import discovery exceeded its execution deadline",
        )
    })?;
    candidates.sort_by(|left, right| {
        (left.kind.as_db_str(), left.provider_session_id.as_str())
            .cmp(&(right.kind.as_db_str(), right.provider_session_id.as_str()))
    });
    candidates.dedup_by(|left, right| {
        left.kind == right.kind && left.provider_session_id == right.provider_session_id
    });
    ensure_before_deadline(deadline).map_err(|error| {
        discovery_error(
            error,
            "agent import discovery exceeded its execution deadline",
        )
    })?;
    let offset = args.cursor.unwrap_or(0);
    if offset > candidates.len() {
        return Err(
            CliError::command_usage("--cursor is outside the discovery result set")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    let end = offset.saturating_add(args.limit).min(candidates.len());
    let next_cursor = (end < candidates.len()).then_some(end);
    Ok((candidates[offset..end].to_vec(), next_cursor))
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
    };
    let repo_root = request.repo_root.into_path_buf();
    let response = match discover(&args, &repo_root, deadline) {
        Ok((candidates, next_cursor)) => DiscoveryHelperResponse::Ok {
            candidates: candidates
                .into_iter()
                .map(|candidate| DiscoveryCandidateWire {
                    kind: candidate.kind.as_cli_slug().to_string(),
                    provider_session_id: candidate.provider_session_id,
                    path: candidate.path.as_deref().map(WirePath::from_path),
                })
                .collect(),
            next_cursor,
        },
        Err(error) => DiscoveryHelperResponse::Error {
            stable_code: error.stable_code().as_str().to_string(),
            message: error.message().to_string(),
        },
    };
    serde_json::to_vec(&response).context("encode internal import discovery response")
}

async fn discover_bounded(
    args: &ImportArgs,
    repo_root: &Path,
    deadline: Instant,
) -> CliResult<(Vec<Candidate>, Option<usize>)> {
    use tokio::io::AsyncWriteExt;

    ensure_before_deadline(deadline).map_err(|error| {
        discovery_error(
            error,
            "agent import discovery exceeded its execution deadline",
        )
    })?;
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
    let program = std::env::current_exe().map_err(|error| {
        CliError::fatal(format!("failed to resolve Libra discovery helper: {error}"))
    })?;
    let mut child = tokio::process::Command::new(program)
        .arg(IMPORT_DISCOVERY_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| CliError::fatal(format!("failed to start import discovery: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CliError::fatal("bounded import discovery helper has no stdin pipe"))?;
    stdin.write_all(&request).await.map_err(|error| {
        CliError::fatal(format!(
            "failed to send bounded import discovery request: {error}"
        ))
    })?;
    drop(stdin);
    let output = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        CliError::fatal("agent import discovery exceeded its total execution deadline")
            .with_stable_code(StableErrorCode::AgentImportPartialBatch)
    })?
    .map_err(|error| CliError::fatal(format!("bounded import discovery failed: {error}")))?;
    if !output.status.success() || output.stdout.len() as u64 > IMPORT_DISCOVERY_HELPER_FRAME_CAP {
        return Err(CliError::fatal(
            "bounded import discovery helper returned an invalid response",
        ));
    }
    let response: DiscoveryHelperResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            CliError::fatal(format!(
                "bounded import discovery returned invalid JSON: {error}"
            ))
        })?;
    match response {
        DiscoveryHelperResponse::Ok {
            candidates,
            next_cursor,
        } => {
            let candidates = candidates
                .into_iter()
                .map(|candidate| {
                    let kind = importable_kind(&candidate.kind)?;
                    Ok(Candidate {
                        kind,
                        provider_session_id: candidate.provider_session_id,
                        path: candidate.path.map(WirePath::into_path_buf),
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;
            Ok((candidates, next_cursor))
        }
        DiscoveryHelperResponse::Error {
            stable_code,
            message,
        } => {
            let error = match stable_code.as_str() {
                "LBR-CLI-002" => CliError::command_usage(message)
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
                "LBR-CLI-003" => {
                    CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidTarget)
                }
                "LBR-AGENT-018" => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::AgentImportPartialBatch),
                "LBR-AGENT-020" => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::AgentTranscriptAuthorizationMissing),
                _ => CliError::fatal("bounded import discovery returned an unknown error code"),
            };
            Err(error)
        }
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

fn require_consent(
    args: &ImportArgs,
    output: &OutputConfig,
    candidate_count: usize,
    deadline: Instant,
) -> CliResult<()> {
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
        args.agent.as_deref().unwrap_or("claude-code/codex"),
        candidate_count,
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

async fn resolve_candidate_source(
    candidate: &Candidate,
    repo_root: &Path,
    deadline: Instant,
) -> Result<(TranscriptSource, String, String)> {
    if candidate.kind == AgentKind::OpenCode {
        let binary = trusted_opencode_binary().await?;
        let session_id = crate::internal::ai::hooks::runtime::build_ai_session_id(
            "opencode",
            &candidate.provider_session_id,
        );
        let source = authorized_sandboxed_export(
            &binary,
            &candidate.provider_session_id,
            &session_id,
            ExportLimits::default(),
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
        ImportError::NoImportableTurns => Some(PreparationImportErrorKind::NoImportableTurns),
        ImportError::BatchInputLimit => Some(PreparationImportErrorKind::BatchInputLimit),
        ImportError::DeadlineExceeded => Some(PreparationImportErrorKind::DeadlineExceeded),
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
    }
}

async fn prepare_candidate_in_helper(
    candidate: &Candidate,
    repo_root: &Path,
    storage_root: &Path,
    read_cap: u64,
    existing_session: Option<&ExistingSessionOwnershipSnapshot>,
    deadline: Instant,
) -> PreparedCandidateOutcome {
    let resolved = resolve_candidate_source(candidate, repo_root, deadline).await;
    let (source, source_kind, source_id) = match resolved {
        Ok(source) => source,
        Err(error) => {
            return PreparedCandidateOutcome {
                request: Err(error),
                raw_bytes: 0,
            };
        }
    };
    if let Err(error) = import_test_pause_after_source_open(deadline) {
        return PreparedCandidateOutcome {
            request: Err(error),
            raw_bytes: 0,
        };
    }
    let read = read_import_source(
        candidate.kind,
        &candidate.provider_session_id,
        source,
        read_cap,
        deadline,
    )
    .await;
    let raw_bytes = read.raw_bytes;
    let content = match read.content {
        Ok(content) => content,
        Err(error) => {
            return PreparedCandidateOutcome {
                request: Err(error),
                raw_bytes,
            };
        }
    };
    let prepared = prepare_import_request(
        candidate.kind,
        &candidate.provider_session_id,
        &source_kind,
        &source_id,
        content,
        ImportPreparationContext {
            current_repo_root: repo_root,
            current_storage_root: storage_root,
            deadline,
        },
    )
    .and_then(|mut prepared| {
        validate_prepared_existing_session(&mut prepared, existing_session)?;
        Ok(prepared)
    });
    PreparedCandidateOutcome {
        request: prepared,
        raw_bytes,
    }
}

async fn run_import_preparation_helper_async(input: &[u8]) -> Result<Vec<u8>> {
    let request: PreparationHelperRequest =
        serde_json::from_slice(input).context("decode internal import preparation request")?;
    if request.read_cap > TRANSCRIPT_READ_HARD_CAP_BYTES || request.remaining_ms == 0 {
        anyhow::bail!("invalid internal import preparation bounds");
    }
    let kind = AgentKind::from_cli_slug(&request.candidate.kind)
        .filter(|kind| {
            matches!(
                kind,
                AgentKind::ClaudeCode | AgentKind::Codex | AgentKind::OpenCode
            )
        })
        .context("invalid internal import preparation agent kind")?;
    let candidate = Candidate {
        kind,
        provider_session_id: request.candidate.provider_session_id.clone(),
        path: request.candidate.path.clone().map(WirePath::into_path_buf),
    };
    let repo_root = request.repo_root.clone().into_path_buf();
    let storage_root = request.storage_root.clone().into_path_buf();
    let deadline = Instant::now()
        + Duration::from_millis(
            request
                .remaining_ms
                .min(IMPORT_TOTAL_DEADLINE.as_millis() as u64),
        );
    let outcome = prepare_candidate_in_helper(
        &candidate,
        &repo_root,
        &storage_root,
        request.read_cap,
        request.existing_session.as_ref(),
        deadline,
    )
    .await;
    let response = match outcome.request {
        Ok(prepared) => PreparationHelperResponse::Ok {
            request: Box::new(prepared),
            raw_bytes: outcome.raw_bytes,
        },
        Err(error) => PreparationHelperResponse::Error {
            error_kind: preparation_error_kind(&error),
            raw_bytes: outcome.raw_bytes,
        },
    };
    serde_json::to_vec(&response).context("encode internal import preparation response")
}

/// Private subprocess entry for the complete post-consent filesystem and
/// normalization boundary. It returns only typed, redacted import state.
#[doc(hidden)]
pub fn run_import_preparation_helper(input: &[u8]) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start internal import preparation runtime")?;
    runtime.block_on(run_import_preparation_helper_async(input))
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
                &request.session_id,
                &request.marker_owner,
                &request.marker_generation,
                &request.agent_kind,
                &request.provider_session_id,
            )
            .await
            {
                Ok(repaired_rows) => IndexRepairHelperResponse::Ok { repaired_rows },
                Err(error) => IndexRepairHelperResponse::Error {
                    message: format!("{error:#}"),
                },
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
    use tokio::io::AsyncWriteExt;

    ensure_before_deadline(deadline)?;
    let frame = serde_json::to_vec(&IndexRepairHelperRequest {
        storage_root: WirePath::from_path(storage_root),
        session_id: barrier.session_id.clone(),
        marker_owner: barrier.marker.owner.clone(),
        marker_generation: barrier.marker.generation.clone(),
        agent_kind: barrier.marker.agent_kind.clone(),
        provider_session_id: barrier.marker.provider_session_id.clone(),
    })
    .context("encode bounded import index repair request")?;
    if frame.len() as u64 > IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP {
        anyhow::bail!("bounded import index repair request exceeds its frame limit");
    }
    let program = std::env::current_exe().context("resolve Libra import index repair helper")?;
    let mut child = tokio::process::Command::new(program)
        .arg(IMPORT_INDEX_REPAIR_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start bounded import index repair helper")?;
    let mut stdin = child
        .stdin
        .take()
        .context("bounded import index repair helper has no stdin pipe")?;
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        stdin.write_all(&frame).await?;
        stdin.shutdown().await
    })
    .await
    .map_err(|_| ImportError::DeadlineExceeded)?
    .context("send bounded import index repair request")?;
    drop(stdin);
    let output = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| ImportError::DeadlineExceeded)?
    .context("wait for bounded import index repair helper")?;
    if !output.status.success() || output.stdout.len() as u64 > IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP
    {
        anyhow::bail!("bounded import index repair helper returned an invalid response");
    }
    match serde_json::from_slice(&output.stdout)
        .context("decode bounded import index repair response")?
    {
        IndexRepairHelperResponse::Ok { repaired_rows } => Ok(repaired_rows),
        IndexRepairHelperResponse::Error { message } => anyhow::bail!(
            "import object-index repair failed: {message}; run `libra agent doctor --repair`"
        ),
    }
}

fn parse_import_index_barrier_marker(value: &str) -> Result<ImportIndexBarrierMarker> {
    let marker: ImportIndexBarrierMarker =
        serde_json::from_str(value).context("decode durable import object-index barrier marker")?;
    if marker.schema_version != 1
        || marker.owner.is_empty()
        || marker.generation.is_empty()
        || marker.identity_id.is_empty()
        || marker.agent_kind.is_empty()
        || marker.provider_session_id.is_empty()
        || !matches!(marker.state.as_str(), "active" | "repair_pending")
    {
        anyhow::bail!(
            "invalid durable import object-index barrier marker; run `libra agent doctor --repair`"
        );
    }
    Ok(marker)
}

async fn lock_import_index_barrier_row<C: ConnectionTrait>(db: &C, session_id: &str) -> Result<()> {
    // This no-op UPDATE is deliberately the first statement in each barrier
    // transaction. On SQLite it obtains the writer slot before we inspect the
    // marker, preventing a read/overwrite race with another process.
    db.execute(Statement::from_sql_and_values(
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

fn marker_is_owned_by(marker: &ImportIndexBarrierMarker, barrier: &ImportIndexBarrier) -> bool {
    marker.owner == barrier.marker.owner
        && marker.generation == barrier.marker.generation
        && marker.identity_id == barrier.marker.identity_id
}

async fn read_owned_import_index_barrier<C: ConnectionTrait>(
    db: &C,
    barrier: &ImportIndexBarrier,
) -> Result<ImportIndexBarrierMarker> {
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
    marker: &ImportIndexBarrierMarker,
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
    let txn = conn
        .begin()
        .await
        .context("begin import object-index partial finalization")?;
    lock_import_index_barrier_row(&txn, &barrier.session_id).await?;
    let mut marker = read_owned_import_index_barrier(&txn, barrier).await?;
    if let Some(identity) = identity {
        if identity.identity_id != marker.identity_id {
            anyhow::bail!("import object-index barrier identity changed before finalization");
        }
        txn.execute(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_import_identity
             SET state = 'partial', last_error_code = 'LBR-AGENT-018', updated_at = ?
             WHERE identity_id = ? AND fence_token = ? AND state = 'committed'",
            [
                Utc::now().timestamp_millis().into(),
                identity.identity_id.clone().into(),
                identity.fence_token.into(),
            ],
        ))
        .await
        .context("mark exact import identity partial after object-index barrier failure")?;
        marker.fence_token = Some(identity.fence_token);
    }
    marker.state = "repair_pending".to_string();
    marker.lease_expires_at = 0;
    persist_import_index_barrier(&txn, &barrier.session_id, &marker).await?;
    txn.commit()
        .await
        .context("commit import object-index partial finalization")?;
    Ok(())
}

async fn clear_import_index_barrier(
    conn: &DatabaseConnection,
    barrier: &ImportIndexBarrier,
) -> Result<()> {
    let txn = conn
        .begin()
        .await
        .context("begin completed import object-index barrier retirement")?;
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
    txn.commit()
        .await
        .context("commit completed import object-index barrier retirement")?;
    Ok(())
}

async fn acquire_import_index_barrier(
    conn: &DatabaseConnection,
    storage_root: &Path,
    request: &ImportRequest,
    deadline: Instant,
) -> Result<ImportIndexBarrier> {
    ensure_before_deadline(deadline)?;
    let now_ms = Utc::now().timestamp_millis();
    let txn = conn
        .begin()
        .await
        .context("begin import object-index barrier acquisition")?;
    lock_import_index_barrier_row(&txn, &request.session_id).await?;
    let tombstone = txn
        .query_one(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                request.agent_kind.as_db_str().into(),
                request.provider_session_id.clone().into(),
            ],
        ))
        .await
        .context("check import tombstone before object-index barrier acquisition")?;
    if tombstone.is_some() {
        txn.rollback().await.ok();
        return Err(ImportError::Erased.into());
    }
    let existing = MetadataKv::get_with_conn(
        &txn,
        MetadataScope::AgentImportIndexRepair,
        &request.session_id,
        IMPORT_INDEX_REPAIR_MARKER_KEY,
    )
    .await
    .context("read prior import object-index barrier marker")?;
    let needs_repair = existing.is_some();
    if let Some(entry) = existing.as_ref() {
        let prior = parse_import_index_barrier_marker(&entry.value)?;
        if prior.state == "active" && prior.lease_expires_at > now_ms {
            txn.rollback().await.ok();
            return Err(ImportError::LeaseBusy.into());
        }
    }
    let marker = ImportIndexBarrierMarker {
        schema_version: 1,
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
    };
    persist_import_index_barrier(&txn, &request.session_id, &marker).await?;
    txn.commit()
        .await
        .context("commit import object-index barrier acquisition")?;
    let barrier = ImportIndexBarrier {
        session_id: request.session_id.clone(),
        marker,
    };
    if needs_repair
        && let Err(error) =
            invoke_import_index_repair_helper(storage_root, &barrier, deadline).await
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
    if cfg!(debug_assertions)
        && std::env::var_os("LIBRA_TEST_IMPORT_INDEX_TOMBSTONE_LOOKUP_FAIL").is_some()
    {
        anyhow::bail!("test-only import index tombstone lookup failure");
    }
    Ok(conn
        .query_one(Statement::from_sql_and_values(
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

fn terminate_import_preparation_helper(
    mut child: tokio::process::Child,
    stdout_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) {
    stdout_task.abort();
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: a negative pid targets the process group created for this
        // helper. SIGKILL bounds descendants that inherited transcript fds or
        // sandbox resources; direct-child start_kill remains the fallback.
        let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.start_kill();
    let reaper = tokio::spawn(async move {
        let _ = child.wait().await;
    });
    drop(reaper);
}

async fn prepare_candidate_bounded(
    candidate: &Candidate,
    repo_root: &Path,
    storage_root: &Path,
    read_cap: u64,
    conn: &sea_orm::DatabaseConnection,
    deadline: Instant,
) -> Result<PreparedCandidateOutcome> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    ensure_before_deadline(deadline)?;
    let existing_session = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        load_existing_session_ownership(conn, candidate.kind, &candidate.provider_session_id),
    )
    .await
    .map_err(|_| ImportError::DeadlineExceeded)??;
    let remaining_ms = u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let helper_request = PreparationHelperRequest {
        candidate: DiscoveryCandidateWire {
            kind: candidate.kind.as_cli_slug().to_string(),
            provider_session_id: candidate.provider_session_id.clone(),
            path: candidate.path.as_deref().map(WirePath::from_path),
        },
        repo_root: WirePath::from_path(repo_root),
        storage_root: WirePath::from_path(storage_root),
        read_cap,
        remaining_ms,
        existing_session,
    };
    let frame =
        serde_json::to_vec(&helper_request).context("encode bounded import preparation request")?;
    if frame.len() as u64 > IMPORT_PREPARATION_HELPER_INPUT_CAP {
        return Err(ImportError::BatchInputLimit.into());
    }
    let program = std::env::current_exe()
        .context("resolve Libra executable for bounded import preparation")?;
    let mut command = tokio::process::Command::new(program);
    command
        .arg(IMPORT_PREPARATION_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .context("start bounded import preparation helper")?;
    let mut stdin = child
        .stdin
        .take()
        .context("bounded import preparation helper has no stdin pipe")?;
    let stdout = child
        .stdout
        .take()
        .context("bounded import preparation helper has no stdout pipe")?;
    let mut stdout_task = tokio::spawn(async move {
        if cfg!(debug_assertions)
            && let Ok(value) = std::env::var("LIBRA_TEST_IMPORT_PREPARATION_RESPONSE_READ_DELAY_MS")
            && let Ok(delay_ms) = value.parse::<u64>()
        {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let mut response = Vec::new();
        stdout
            .take(IMPORT_PREPARATION_HELPER_OUTPUT_CAP.saturating_add(1))
            .read_to_end(&mut response)
            .await?;
        Ok(response)
    });
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        stdin.write_all(&frame),
    )
    .await
    {
        Ok(Ok(())) => drop(stdin),
        Ok(Err(error)) => {
            terminate_import_preparation_helper(child, stdout_task);
            return Err(error).context("send bounded import preparation request");
        }
        Err(_) => {
            drop(stdin);
            terminate_import_preparation_helper(child, stdout_task);
            return Err(ImportError::DeadlineExceeded.into());
        }
    }
    let status =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), child.wait()).await
        {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                terminate_import_preparation_helper(child, stdout_task);
                return Err(error).context("wait for import preparation helper");
            }
            Err(_) => {
                terminate_import_preparation_helper(child, stdout_task);
                return Err(ImportError::DeadlineExceeded.into());
            }
        };
    ensure_before_deadline(deadline)?;
    if !status.success() {
        stdout_task.abort();
        anyhow::bail!("bounded import preparation helper exited unsuccessfully");
    }
    let response_bytes =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut stdout_task)
            .await
        {
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(error))) => {
                return Err(error).context("read bounded import preparation response");
            }
            Ok(Err(error)) => {
                return Err(error).context("join bounded import preparation response reader");
            }
            Err(_) => {
                stdout_task.abort();
                return Err(ImportError::DeadlineExceeded.into());
            }
        };
    if response_bytes.len() as u64 > IMPORT_PREPARATION_HELPER_OUTPUT_CAP {
        anyhow::bail!("bounded import preparation helper exceeded its response limit");
    }
    ensure_before_deadline(deadline)?;
    let response: PreparationHelperResponse = serde_json::from_slice(&response_bytes)
        .context("decode bounded import preparation response")?;
    Ok(match response {
        PreparationHelperResponse::Ok { request, raw_bytes } => PreparedCandidateOutcome {
            request: Ok(*request),
            raw_bytes,
        },
        PreparationHelperResponse::Error {
            error_kind,
            raw_bytes,
        } => PreparedCandidateOutcome {
            request: Err(match error_kind {
                Some(kind) => preparation_error(kind).into(),
                None => anyhow::anyhow!("bounded import preparation rejected the source"),
            }),
            raw_bytes,
        },
    })
}

fn stable_code_for_error(error: &anyhow::Error) -> StableErrorCode {
    match error.downcast_ref::<ImportError>() {
        Some(ImportError::RepositoryConflict | ImportError::SessionIdentityConflict) => {
            StableErrorCode::AgentImportRepositoryConflict
        }
        Some(ImportError::WorkingDirMissingOrAmbiguous) => {
            StableErrorCode::AgentImportWorkingDirInvalid
        }
        Some(ImportError::Erased) => StableErrorCode::AgentImportErased,
        Some(ImportError::SourceAuthorization) => {
            StableErrorCode::AgentTranscriptAuthorizationMissing
        }
        Some(
            ImportError::LeaseBusy
            | ImportError::NoImportableTurns
            | ImportError::BatchInputLimit
            | ImportError::DeadlineExceeded,
        )
        | None => StableErrorCode::AgentImportPartialBatch,
    }
}

fn safe_failure(candidate: &Candidate, error: &anyhow::Error) -> BatchFailure {
    let digest = sha2::Sha256::digest(candidate.provider_session_id.as_bytes());
    BatchFailure {
        status: "failed",
        agent_kind: candidate.kind.as_db_str().to_string(),
        session_id: format!("sha256:{}", hex::encode(&digest[..6])),
        error_code: stable_code_for_error(error),
    }
}

fn safe_skip(candidate: &Candidate, error: &anyhow::Error) -> BatchSkip {
    let failure = safe_failure(candidate, error);
    BatchSkip {
        status: "skipped",
        agent_kind: failure.agent_kind,
        session_id: failure.session_id,
        reason_code: failure.error_code,
    }
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

pub async fn execute_safe(args: ImportArgs, output: &OutputConfig) -> CliResult<()> {
    let deadline = Instant::now() + import_total_deadline();
    let repo_root = util::try_working_dir().map_err(|_| CliError::repo_not_found())?;
    let storage_root = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    let (candidates, next_cursor) = discover_bounded(&args, &repo_root, deadline).await?;
    require_consent(&args, output, candidates.len(), deadline)?;
    let conn = db::get_db_conn_instance_for_path(&storage_root.join(util::DATABASE))
        .await
        .map_err(|error| CliError::fatal(format!("failed to open repository database: {error}")))?;
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
    let mut skipped = Vec::new();
    let mut failures = Vec::new();
    let discovered_batch = args.all || args.since.is_some();
    let mut cumulative_raw_bytes = 0_u64;
    let raw_byte_cap = batch_raw_byte_cap();
    for candidate in &candidates {
        if Instant::now() >= deadline {
            failures.push(safe_failure(
                candidate,
                &ImportError::DeadlineExceeded.into(),
            ));
            continue;
        }
        let tombstoned =
            session_is_tombstoned(&conn, candidate.kind, &candidate.provider_session_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!("failed to check import tombstone: {error}"))
                })?;
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
            let prepared = prepare_candidate_bounded(
                candidate,
                &repo_root,
                &storage_root,
                remaining_raw_bytes,
                &conn,
                deadline,
            )
            .await?;
            cumulative_raw_bytes = cumulative_raw_bytes
                .checked_add(prepared.raw_bytes)
                .ok_or(ImportError::BatchInputLimit)?;
            if cumulative_raw_bytes > raw_byte_cap {
                return Err(ImportError::BatchInputLimit.into());
            }
            let request = prepared.request?;
            let barrier = acquire_import_index_barrier(
                &conn,
                &storage_root,
                &request,
                deadline,
            )
            .await?;
            index_barrier = Some(barrier);
            import_test_pause_after_index_barrier(deadline)?;
            let subagent_discovery = if request.agent_kind == AgentKind::ClaudeCode {
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
                let discovery_result = crate::internal::ai::subagent_content::discover_claude_subagent_contents_bounded(
                        &request.working_dir,
                        &request.provider_session_id,
                        discovery_deadline,
                        subagent_budget,
                        crate::internal::ai::subagent_content::MAX_SUBAGENT_SOURCES_PER_CAPTURE,
                    )
                    .await;
                match discovery_result {
                    Ok(discovery) => {
                        settle_subagent_input_allowance(
                            &mut cumulative_raw_bytes,
                            subagent_budget,
                            discovery.bytes_read,
                        )?;
                        discovery
                    }
                    Err(error) => match crate::internal::ai::subagent_content::SubagentDiscovery::from_deadline_error(&error) {
                        Some(discovery) => discovery,
                        None => return Err(error),
                    },
                }
            } else {
                crate::internal::ai::subagent_content::SubagentDiscovery::default()
            };
            import_prepared_with_subagent_discovery(
                &conn,
                &storage_root,
                request,
                deadline,
                subagent_discovery,
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
        record_candidate_result(
            candidate,
            discovered_batch,
            index_incomplete,
            result,
            &mut results,
            &mut partial_results,
            &mut skipped,
            &mut failures,
        );
    }

    let payload = BatchOutput {
        schema_version: 1,
        results,
        partial_results,
        skipped,
        failures,
        next_cursor,
    };
    if !payload.failures.is_empty() {
        let stable_code = if candidates.len() == 1
            && payload.results.is_empty()
            && payload.partial_results.is_empty()
            && payload.failures.len() == 1
        {
            payload.failures[0].error_code
        } else {
            StableErrorCode::AgentImportPartialBatch
        };
        let message = if candidates.len() == 1
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
        let mut error = CliError::fatal(message)
            .with_stable_code(stable_code)
            .with_detail("schema_version", payload.schema_version)
            .with_detail("succeeded", payload.results.len())
            .with_detail("partial", payload.partial_results.len())
            .with_detail("skipped", payload.skipped.len())
            .with_detail("failed", payload.failures.len())
            .with_detail("next_cursor", payload.next_cursor);
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
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
        assert!(failures[0].session_id.starts_with("sha256:"));
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

    struct TestHomeGuard(Option<std::ffi::OsString>);

    impl TestHomeGuard {
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
        };
        assert!(
            discover(
                &args,
                Path::new("."),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
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
    #[serial_test::serial]
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
