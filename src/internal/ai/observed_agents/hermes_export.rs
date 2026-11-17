//! Live read bridge for the Hermes source selected by HM-00.
//!
//! JSONL and SQLite go through the HM-03 reader. The exporter branch runs
//! only after this module holds the existing export-job lease, and only with
//! the frozen argv `sessions export --format jsonl --session-id <id>
//! <output-file>`. OpenCode trust is never consulted.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use sea_orm::{ConnectionTrait, DatabaseConnection};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

use super::{
    builtin::hermes::{
        HermesReadLimits, HermesScope, HermesSourceRequest, HermesSourceStatus, HermesTranscript,
        hermes_worker_frame, read_hermes_source,
    },
    trust::{self, HERMES_EXPORTER_SLUG, TrustRecord},
};
use crate::internal::ai::{
    capture_scope::{CaptureCommitDeadline, CaptureScope},
    export_job::{self, IdleOutcome},
};

/// Live hook budget from the four-provider contract. A later explicit retry
/// uses a new deadline; this one is not extended in place.
/// Child budget for `hermes sessions export`. A v0.22.5 export of one local
/// session measured about 12s, so the old 2s budget killed the process
/// before it wrote JSONL. 20s stays inside the 30s capture wrapper.
pub const HERMES_LIVE_DEADLINE: Duration = Duration::from_secs(20);

const EXPORT_ARGV: [&str; 5] = ["sessions", "export", "--format", "jsonl", "--session-id"];

/// Executable trust loaded for the Hermes exporter slug only.
#[derive(Debug, Clone)]
pub struct HermesExporterTrust {
    record: TrustRecord,
}

impl HermesExporterTrust {
    /// Load the Hermes exporter record. An OpenCode record is not accepted.
    pub async fn load<C: ConnectionTrait>(conn: &C) -> anyhow::Result<Option<Self>> {
        Ok(trust::read_trust_with_conn(conn, HERMES_EXPORTER_SLUG)
            .await?
            .map(|record| Self { record }))
    }

    pub fn path(&self) -> &Path {
        &self.record.path
    }
}

/// Which already-selected source this read uses.
#[derive(Debug, Clone)]
pub enum HermesLiveSource {
    /// Run the trusted Hermes CLI. The output path is chosen here and must
    /// not already exist.
    Export,
    /// Official JSONL already on disk. No process is started.
    Jsonl { path: PathBuf },
    /// Provider SQLite. The HM-03 reader opens it read-only.
    Sqlite { path: PathBuf },
}

/// Caller-recorded session. Profile and cwd come from the caller, not from
/// a global default or an event guess.
#[derive(Debug, Clone)]
pub struct HermesLiveRequest {
    pub session_id: String,
    pub profile: String,
    pub cwd: PathBuf,
    pub source: HermesLiveSource,
}

/// Source-qualified ordinary read. Native origin is never claimed.
#[derive(Debug, Clone)]
pub struct HermesLiveRead {
    pub partial: bool,
    pub source_qualified: bool,
    pub native_claimed: bool,
    pub source_state: &'static str,
    pub spawned: bool,
    pub readable_thinking: Vec<String>,
    /// Original selected export bytes. Empty when the read did not keep a frame.
    pub transcript: Vec<u8>,
}

impl HermesLiveRead {
    fn empty(partial: bool, spawned: bool) -> Self {
        Self {
            partial,
            source_qualified: true,
            native_claimed: false,
            source_state: "unproven",
            spawned,
            readable_thinking: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

/// Content-free failure. Display never includes exporter stdout, stderr, or
/// thinking text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HermesLiveError {
    NoLease,
    Unavailable,
    BadRequest,
}

impl std::fmt::Display for HermesLiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoLease => "hermes export did not start without the export lease",
            Self::Unavailable => "hermes export is unavailable",
            Self::BadRequest => "hermes live read request is not usable",
        })
    }
}

impl std::error::Error for HermesLiveError {}

/// Lease held in `agent_export_job` for this Hermes session.
pub struct HermesExportLease {
    owner: String,
    fence_token: i64,
    session_id: String,
}

impl HermesExportLease {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Become the export-job runner, or return `None` when another runner holds
/// the lease. A database failure is unavailable and does not start a process.
pub async fn acquire_hermes_export_lease(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    deadline: Instant,
) -> Result<Option<HermesExportLease>, HermesLiveError> {
    if !session_id_is_safe(session_id) || Instant::now() >= deadline {
        return Err(HermesLiveError::BadRequest);
    }
    let commit_deadline = commit_deadline_until(deadline)?;
    let owner = format!("hermes-live-{}", std::process::id());
    let outcome = export_job::observe_idle_until(
        conn,
        "hermes",
        session_id,
        scope,
        &owner,
        chrono::Utc::now().timestamp_millis(),
        Some(commit_deadline),
    )
    .await
    .map_err(|_| HermesLiveError::Unavailable)?;
    match outcome {
        IdleOutcome::Runner { fence_token, .. } => Ok(Some(HermesExportLease {
            owner,
            fence_token,
            session_id: session_id.to_string(),
        })),
        IdleOutcome::RecordedOnly => Ok(None),
    }
}

pub async fn release_hermes_export_lease(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    lease: HermesExportLease,
) -> Result<(), HermesLiveError> {
    export_job::release(
        conn,
        &export_job::ExportJobTarget::new("hermes", &lease.session_id, scope),
        &lease.owner,
        lease.fence_token,
        "idle",
        None,
        chrono::Utc::now().timestamp_millis(),
    )
    .await
    .map_err(|_| HermesLiveError::Unavailable)
}

/// Read the selected source. `None` means this caller does not hold the
/// export lease, so no process starts and no provider file is opened.
pub async fn read_hermes_live_source(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    lease: Option<HermesExportLease>,
    trust: Option<&HermesExporterTrust>,
    request: &HermesLiveRequest,
    deadline: Instant,
) -> Result<HermesLiveRead, HermesLiveError> {
    let Some(lease) = lease else {
        return Err(HermesLiveError::NoLease);
    };
    if lease.session_id != request.session_id || !request_is_safe(request) {
        release_hermes_export_lease(conn, scope, lease).await.ok();
        return Err(HermesLiveError::BadRequest);
    }
    let read = match &request.source {
        HermesLiveSource::Export => {
            let Some(trust) = trust else {
                release_hermes_export_lease(conn, scope, lease).await.ok();
                return Err(HermesLiveError::Unavailable);
            };
            export_under_lease(conn, trust, request, deadline).await
        }
        HermesLiveSource::Jsonl { path } | HermesLiveSource::Sqlite { path } => {
            read_selected_file(request, path, deadline).await
        }
    };
    release_hermes_export_lease(conn, scope, lease).await.ok();
    read
}

async fn export_under_lease(
    conn: &DatabaseConnection,
    trust: &HermesExporterTrust,
    request: &HermesLiveRequest,
    deadline: Instant,
) -> Result<HermesLiveRead, HermesLiveError> {
    let directory = tempfile::tempdir().map_err(|_| HermesLiveError::Unavailable)?;
    let output = directory.path().join("export.jsonl");
    if output.exists() {
        return Err(HermesLiveError::Unavailable);
    }
    // The lease is on `conn`; the trust row is in that same repository's
    // `libra.db` (`conn` itself when it is a combined database). The process
    // repository can be a different checkout, so revocation is read from the
    // database paired with `conn`, never from the current directory.
    let repository = crate::internal::db::repository_peer(conn)
        .await
        .map_err(|_| HermesLiveError::Unavailable)?;
    trust::revalidate_trust_with_conn(&repository, HERMES_EXPORTER_SLUG, &trust.record)
        .await
        .map_err(|_| HermesLiveError::Unavailable)?;
    let mut child = Command::new(&trust.record.path);
    child
        .args(EXPORT_ARGV)
        .arg(&request.session_id)
        .arg(&output)
        .current_dir(&request.cwd)
        .env_clear()
        .env("PWD", &request.cwd)
        .env("PATH", "/usr/bin:/bin");
    // The account default profile is a different database. Forward only an
    // absolute HERMES_HOME so an isolated profile is the one that is read.
    if let Some(home) = forwarded_hermes_home() {
        child.env("HERMES_HOME", home);
    }
    child
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn().map_err(|_| HermesLiveError::Unavailable)?;
    let budget = HERMES_LIVE_DEADLINE.min(deadline.saturating_duration_since(Instant::now()));
    if budget.is_zero() {
        child.start_kill().ok();
        return Ok(HermesLiveRead::empty(true, true));
    }
    let finished = timeout(budget, child.wait()).await;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let status = match finished {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            child.start_kill().ok();
            return Ok(HermesLiveRead::empty(true, true));
        }
        Err(_) => {
            child.start_kill().ok();
            let _ = timeout(Duration::from_millis(200), child.wait()).await;
            return Ok(HermesLiveRead::empty(true, true));
        }
    };
    let stdout_text = drain_pipe(&mut stdout).await;
    let _stderr = drain_pipe(&mut stderr).await;
    if !status.success()
        || stdout_signals_unavailable(&stdout_text)
        || !output.is_file()
        || fs::metadata(&output)
            .map(|meta| meta.len() == 0)
            .unwrap_or(true)
    {
        return Ok(HermesLiveRead::empty(true, true));
    }
    let limits = HermesReadLimits::default();
    let length = fs::metadata(&output)
        .map(|meta| meta.len())
        .map_err(|_| HermesLiveError::Unavailable)?;
    if length == 0 || length > limits.max_bytes {
        return Ok(HermesLiveRead::empty(true, true));
    }
    let bytes = fs::read(&output).map_err(|_| HermesLiveError::Unavailable)?;
    finish_bytes(&bytes, request, deadline, true)
}

async fn read_selected_file(
    request: &HermesLiveRequest,
    path: &Path,
    deadline: Instant,
) -> Result<HermesLiveRead, HermesLiveError> {
    if Instant::now() >= deadline {
        return Ok(HermesLiveRead::empty(true, false));
    }
    let scope = live_scope(request)?;
    let limits = HermesReadLimits::default();
    let source_request = HermesSourceRequest {
        authorized_root: request.cwd.clone(),
        jsonl: matches!(request.source, HermesLiveSource::Jsonl { .. }).then(|| path.to_path_buf()),
        sqlite: matches!(request.source, HermesLiveSource::Sqlite { .. })
            .then(|| path.to_path_buf()),
        scope,
        limits,
    };
    match read_hermes_source(&source_request).await {
        HermesSourceStatus::Read(transcript) => finish_transcript(transcript, false),
        HermesSourceStatus::Unavailable => Ok(HermesLiveRead::empty(true, false)),
    }
}

fn finish_bytes(
    bytes: &[u8],
    request: &HermesLiveRequest,
    deadline: Instant,
    spawned: bool,
) -> Result<HermesLiveRead, HermesLiveError> {
    let scope = live_scope(request)?;
    let frame = hermes_worker_frame(bytes, &scope, Some(deadline))
        .map_err(|_| HermesLiveError::Unavailable)?;
    Ok(HermesLiveRead {
        partial: true,
        source_qualified: true,
        native_claimed: false,
        source_state: frame.source_state,
        spawned,
        readable_thinking: frame
            .thinking
            .iter()
            .filter_map(|item| item.readable_thinking().map(str::to_string))
            .collect(),
        transcript: bytes.to_vec(),
    })
}

fn finish_transcript(
    transcript: HermesTranscript,
    spawned: bool,
) -> Result<HermesLiveRead, HermesLiveError> {
    Ok(HermesLiveRead {
        partial: transcript.partial,
        source_qualified: true,
        native_claimed: false,
        source_state: "unproven",
        spawned,
        readable_thinking: transcript
            .messages
            .iter()
            .filter_map(|message| message.readable_thinking().map(str::to_string))
            .collect(),
        transcript: Vec::new(),
    })
}

fn live_scope(request: &HermesLiveRequest) -> Result<HermesScope, HermesLiveError> {
    if !request_is_safe(request) {
        return Err(HermesLiveError::BadRequest);
    }
    Ok(HermesScope {
        session_id: request.session_id.clone(),
        profile: request.profile.clone(),
        repo_root: request.cwd.clone(),
    })
}

fn request_is_safe(request: &HermesLiveRequest) -> bool {
    session_id_is_safe(&request.session_id)
        && !request.profile.is_empty()
        && !request.profile.contains(['/', '\\'])
        && request.cwd.is_absolute()
}

fn session_id_is_safe(session_id: &str) -> bool {
    // One argv token. Hook admission allows 128 characters. A Hermes local
    // id is `local-` plus 64 hex digits, which is 70 characters.
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Absolute `HERMES_HOME` only. A missing or relative value is not forwarded,
/// so the child cannot be pointed at a path the operator did not name.
fn forwarded_hermes_home() -> Option<std::ffi::OsString> {
    let value = std::env::var_os("HERMES_HOME")?;
    let text = value.to_str()?;
    if text.is_empty() || text.contains(['\0', '\n', '\r']) || !Path::new(text).is_absolute() {
        return None;
    }
    Some(value)
}

fn stdout_signals_unavailable(stdout: &str) -> bool {
    stdout.contains("No session")
        || stdout.contains("No sessions found")
        || stdout.to_ascii_lowercase().contains("migration")
}

async fn drain_pipe(pipe: &mut Option<impl AsyncReadExt + Unpin>) -> String {
    let Some(pipe) = pipe.as_mut() else {
        return String::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.take(16 * 1024).read_to_end(&mut buffer).await;
    String::from_utf8_lossy(&buffer).into_owned()
}

fn commit_deadline_until(deadline: Instant) -> Result<CaptureCommitDeadline, HermesLiveError> {
    let budget = deadline
        .saturating_duration_since(Instant::now())
        .min(HERMES_LIVE_DEADLINE);
    if budget.is_zero() {
        return Err(HermesLiveError::BadRequest);
    }
    CaptureCommitDeadline::from_budget(budget).map_err(|_| HermesLiveError::Unavailable)
}

/// Doctor fact for the Hermes exporter. It reports trust and the
/// source-qualified boundary, never a native-origin claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct HermesBridgeStatus {
    pub trusted: bool,
    pub source_qualified: bool,
    pub native_claimed: bool,
}

impl HermesBridgeStatus {
    /// Read the process-global Hermes exporter trust record.
    pub async fn current() -> anyhow::Result<Self> {
        Ok(Self {
            trusted: trust::read_trust(HERMES_EXPORTER_SLUG).await?.is_some(),
            source_qualified: true,
            native_claimed: false,
        })
    }
}

impl std::fmt::Display for HermesBridgeStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Hermes export bridge: trusted={}; source-qualified; native origin is not claimed; review and investigate stay closed; cloud sync stays refused",
            if self.trusted { "yes" } else { "no" }
        )
    }
}

/// The fixed exporter arguments, without the session id or output path.
pub fn hermes_export_argv() -> &'static [&'static str] {
    &EXPORT_ARGV
}

/// Live snapshot entry. The capture layer calls this before it admits its own
/// export runner, so this function can take the export-job lease itself.
pub async fn export_for_live_capture(
    session_id: &str,
    cwd: &Path,
    deadline: Instant,
) -> Result<HermesLiveRead, HermesLiveError> {
    if !session_id_is_safe(session_id) || !cwd.is_absolute() || Instant::now() >= deadline {
        return Err(HermesLiveError::BadRequest);
    }
    let storage = crate::utils::util::try_get_storage_path(Some(cwd.to_path_buf()))
        .map_err(|_| HermesLiveError::Unavailable)?;
    let database = storage.join(crate::utils::util::DATABASE);
    let conn = crate::internal::db::get_db_conn_instance_for_path(&database)
        .await
        .map_err(|_| HermesLiveError::Unavailable)?;
    let scope = CaptureScope::resolve(&conn, cwd)
        .await
        .map_err(|_| HermesLiveError::Unavailable)?;
    let trust = HermesExporterTrust::load(&conn)
        .await
        .map_err(|_| HermesLiveError::Unavailable)?;
    let Some(trust) = trust else {
        return Err(HermesLiveError::Unavailable);
    };
    // The export-job lease is a catalog row: after the split it is in
    // `capture.db`. A repository without one keeps the combined database.
    let remaining = deadline.saturating_duration_since(Instant::now());
    let conn = if crate::internal::ai::capture::store::capture_database_path(&storage).is_file() {
        crate::internal::ai::capture::store::open_existing_writer(
            &storage,
            remaining.min(crate::internal::ai::capture::store::COMMAND_BUSY_TIMEOUT),
        )
        .await
        .map_err(|_| HermesLiveError::Unavailable)?
    } else {
        conn
    };
    let request = HermesLiveRequest {
        session_id: session_id.to_string(),
        profile: "default".to_string(),
        cwd: cwd.to_path_buf(),
        source: HermesLiveSource::Export,
    };
    let lease = acquire_hermes_export_lease(&conn, &scope, session_id, deadline).await?;
    read_hermes_live_source(&conn, &scope, lease, Some(&trust), &request, deadline).await
}
