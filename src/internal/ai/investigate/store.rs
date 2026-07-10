//! Investigate run directory store (`.libra/sessions/agent-runs/<run_id>/`).
//!
//! Persists the E8-libra run wire for the AG-23 strict-round-robin
//! investigate workflow (plan.md Task A8; `docs/development/tracing/agent.md`
//! E8-libra + 落地执行补充规格 §5). Shares the run-directory layout with
//! [`crate::internal::ai::review`] — same `state.json` / `manifest.json` /
//! `findings.md` / `reviewers/<slug>.{stdout,stderr}.redacted.log` file
//! set, same keyset ordering, same tolerant reads — but carries the
//! round-robin state fields (`turn`, `next_agent_idx`, `completed_rounds`,
//! `quorum`, `stances`, `pending_turn`, …) that review has no use for.
//!
//! # Single-writer contract
//!
//! Unlike review's concurrent fan-in, investigate is strictly serial: one
//! investigator runs at a time and exactly one process drives a run's
//! turns at once. That single-writer invariant is enforced at the OS level
//! by [`InvestigateRunStore::try_lock_run`] (an `flock`-based run lock);
//! `findings.md` is therefore written by a single producer with a
//! whole-file snapshot overwrite.

use std::{
    fs::{self, File},
    io::{self},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::internal::ai::review::store::{AGENT_FINDINGS_OTYPE, read_json_opt, utc_timestamp};
// The run-id / reviewer-name validators and the redaction summary are
// shared verbatim with the review store (same path-traversal guard, same
// filesystem-safe log naming, same redaction accounting).
pub use crate::internal::ai::review::store::{
    RedactionReportSummary, is_valid_run_id, sanitize_reviewer_name,
};

/// `manifest.json` schema version (E8-libra manifest contract).
pub const INVESTIGATE_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The `kind` value investigate runs stamp into `state.json` /
/// `manifest.json` (`review|investigate` per E8-libra; review owns
/// `review`).
pub const INVESTIGATE_RUN_KIND: &str = "investigate";

/// Canonical findings document name recorded in `findings_doc`.
pub const INVESTIGATE_FINDINGS_DOC: &str = "findings.md";

/// Directory under the sessions root holding one subdirectory per run
/// (shared with review — a run id is unique across both kinds).
const AGENT_RUNS_DIR: &str = "agent-runs";
/// Cross-process cancel-request marker file inside a run directory.
const CANCEL_REQUESTED_FILE: &str = "cancel.requested";
/// Per-run OS lock file (`flock`-guarded; see [`InvestigateRunStore::try_lock_run`]).
const RUN_LOCK_FILE: &str = ".lock";

// ---------------------------------------------------------------------------
// Terminal states / pause reasons / stance dispositions
// ---------------------------------------------------------------------------

/// The terminal states an investigate run can END in. A run that PAUSES
/// (resumable via `investigate continue`) has `terminal_state == None`
/// and a `pending_turn` instead — see [`PauseReason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigateTerminalState {
    /// At least `quorum` distinct investigators submitted a concluding
    /// stance — the investigation converged.
    Quorum,
    /// The turn budget (`max_turns`) was exhausted before quorum. Whether
    /// this reads as "success" or "partial" is informational and derives
    /// from whether any findings were recorded (documented in the CLI /
    /// docs); the terminal state itself is `max_turns`.
    MaxTurns,
    /// The run was cancelled (`investigate cancel` / SIGINT / SIGTERM —
    /// one shared cleanup path).
    Cancelled,
    /// The run-level wall-clock budget (`max_turns * 120s`, capped at
    /// 3600s per `agent.md` 强制补强项 #11) was exceeded — fail-closed,
    /// every process/lock/workspace released.
    Timeout,
    /// Infrastructure failure (workspace/store) before or between turns.
    Error,
}

impl InvestigateTerminalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quorum => "quorum",
            Self::MaxTurns => "max_turns",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for InvestigateTerminalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a run PAUSED (non-terminal): the recorded `pending_turn` names the
/// turn `investigate continue` will retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// The investigator produced no new findings (empty output on a
    /// successful turn) — the investigation stalled.
    Stalled,
    /// The investigator failed to launch, exited non-zero, or hit its
    /// per-turn deadline. `continue` retries the same turn.
    AgentFailure,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stalled => "stalled",
            Self::AgentFailure => "agent_failure",
        }
    }
}

impl std::fmt::Display for PauseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The disposition an investigator's stance signals for a turn. Detected
/// from the (redacted) stdout by [`classify_stance_disposition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StanceDisposition {
    /// The investigator signalled a conclusion (counts toward quorum).
    Concluding,
    /// The investigator wants the investigation to continue.
    Continuing,
}

impl StanceDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Concluding => "concluding",
            Self::Continuing => "continuing",
        }
    }
}

/// Classify a stance from an investigator's redacted stdout.
///
/// Conservative, deterministic rule (the quorum semantics `agent.md` §5
/// leaves to the implementation): a stance is [`StanceDisposition::Concluding`]
/// when its output contains the case-insensitive token `conclud` (matching
/// "conclude" / "concluding" / an explicit `STANCE: concluding` marker);
/// otherwise it is [`StanceDisposition::Continuing`]. Emptiness is handled
/// upstream (an empty successful turn is a *stall*, not a stance).
pub fn classify_stance_disposition(stdout: &str) -> StanceDisposition {
    if stdout.to_ascii_lowercase().contains("conclud") {
        StanceDisposition::Concluding
    } else {
        StanceDisposition::Continuing
    }
}

// ---------------------------------------------------------------------------
// Stance / pending-turn records
// ---------------------------------------------------------------------------

/// One investigator turn's recorded stance (the single-writer `stances`
/// list in `state.json`). The `summary` is a redacted, ANSI-safe excerpt
/// of the investigator's stdout — provenance=untrusted, always rendered
/// through the sink's ANSI stripper before display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StanceEntry {
    /// 1-based turn number this stance was produced on.
    pub turn: u32,
    /// Index into `agents` (round-robin position).
    pub agent_idx: usize,
    pub slug: String,
    pub disposition: StanceDisposition,
    /// Redacted, control-scrubbed one-block excerpt (provenance=untrusted).
    pub summary: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout_truncated: bool,
}

/// The turn a paused run will retry on `investigate continue`. Present iff
/// the run paused (`terminal_state == None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTurn {
    pub turn: u32,
    pub agent_idx: usize,
    pub slug: String,
    pub reason: PauseReason,
    /// Redacted single-line detail (e.g. the launch/exit failure) — never
    /// raw stderr.
    #[serde(default)]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Manifest (E8 exact key set) — kind = "investigate"
// ---------------------------------------------------------------------------

/// `manifest.json` — **exactly** the E8-libra 12-key set
/// (`agent.md:876` / `agent.md:1321`), with `kind = "investigate"`. The
/// key-set exactness is pinned by a unit test below; do not add or rename
/// fields without updating the E8 spec first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvestigateManifest {
    pub schema_version: u32,
    pub run_id: String,
    /// Always `"investigate"` ([`INVESTIGATE_RUN_KIND`]).
    pub kind: String,
    /// Investigator slugs in round-robin order.
    pub agents: Vec<String>,
    pub starting_sha: String,
    /// Human-readable investigate scope (the topic label).
    pub target_scope: String,
    /// `null` while the run is in flight or PAUSED; exactly one terminal
    /// state once it ends.
    pub terminal_state: Option<InvestigateTerminalState>,
    /// RFC 3339 UTC, fixed microsecond precision (lexicographic order ==
    /// chronological order — the keyset pagination contract relies on it).
    pub created_at: String,
    pub updated_at: String,
    /// OID of `findings.md` in the object store. `null` at create time / for
    /// empty findings; populated at terminal manifest write (A0-06) with a
    /// content-addressed, `object_index`-visible, doctor-repairable blob.
    pub findings_oid: Option<String>,
    pub redaction_report: RedactionReportSummary,
    /// Manual attachments (A0-06): `{oid, path, provenance:"manual", size,
    /// attached_at}` per external file attached via `libra investigate
    /// attach`. Empty until the first attach; attached bytes are redacted and
    /// object_index-tagged like findings.
    pub manual_attach: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// State (engine-internal, carries the E8-entire round-robin fields)
// ---------------------------------------------------------------------------

/// `state.json` — the investigate run's persisted state, carrying every
/// E8-entire round-robin field (`agent.md:869` / plan.md:995):
/// `run_id`, `topic`, `agents`, `max_turns`, `quorum`, `completed_rounds`,
/// `turn`, `next_agent_idx`, `stances`, `findings_doc`, `starting_sha`,
/// `started_at`, `updated_at`, `pending_turn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvestigateRunState {
    pub schema_version: u32,
    pub run_id: String,
    /// The investigation topic — untrusted seed text (plan.md:998). Stored
    /// verbatim for display; ALWAYS sanitized on render and redacted +
    /// spotlit before any prompt injection.
    pub topic: String,
    pub kind: String,
    /// Investigator slugs in strict round-robin order.
    pub agents: Vec<String>,
    pub max_turns: u32,
    pub quorum: u32,
    /// Number of complete round-robin rounds finished.
    pub completed_rounds: u32,
    /// Number of turns that produced a stance so far.
    pub turn: u32,
    /// Index into `agents` of the next investigator to run.
    pub next_agent_idx: usize,
    pub stances: Vec<StanceEntry>,
    /// Relative name of the single-writer findings document
    /// ([`INVESTIGATE_FINDINGS_DOC`]).
    pub findings_doc: String,
    pub starting_sha: String,
    pub started_at: String,
    pub updated_at: String,
    /// Set iff the run PAUSED and is resumable via `investigate continue`.
    #[serde(default)]
    pub pending_turn: Option<PendingTurn>,
    /// `null` while running/paused; exactly one terminal state at the end.
    #[serde(default)]
    pub terminal_state: Option<InvestigateTerminalState>,
    /// Set when a cross-process cancel request was observed.
    #[serde(default)]
    pub cancel_requested: bool,
    /// Root of the run's materialized isolated workspace (recorded before
    /// investigators spawn so an orphaned run can be cleaned up).
    #[serde(default)]
    pub workspace_root: Option<String>,
}

impl InvestigateRunState {
    pub fn is_terminal(&self) -> bool {
        self.terminal_state.is_some()
    }

    /// Whether the run is paused (non-terminal with a recorded
    /// `pending_turn`).
    pub fn is_paused(&self) -> bool {
        self.terminal_state.is_none() && self.pending_turn.is_some()
    }

    /// Distinct investigators (by slug) that have submitted at least one
    /// concluding stance.
    pub fn concluding_agent_count(&self) -> usize {
        let mut seen: Vec<&str> = Vec::new();
        for stance in &self.stances {
            if stance.disposition == StanceDisposition::Concluding
                && !seen.contains(&stance.slug.as_str())
            {
                seen.push(&stance.slug);
            }
        }
        seen.len()
    }
}

// ---------------------------------------------------------------------------
// Listing / keyset pagination
// ---------------------------------------------------------------------------

/// One row of `investigate list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvestigateRunSummary {
    pub run_id: String,
    pub kind: String,
    pub agents: Vec<String>,
    pub topic: String,
    pub turn: u32,
    pub max_turns: u32,
    pub terminal_state: Option<InvestigateTerminalState>,
    /// Present iff paused (resumable).
    pub pause_reason: Option<PauseReason>,
    pub started_at: String,
    pub updated_at: String,
}

/// Keyset cursor for `investigate list` pagination: strictly-after
/// position in (`started_at DESC`, `run_id DESC`) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestigateRunCursor {
    pub started_at: String,
    pub run_id: String,
}

/// One keyset page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvestigateRunPage {
    pub items: Vec<InvestigateRunSummary>,
    pub next_cursor: Option<InvestigateRunCursor>,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Run lock (OS-level; flock)
// ---------------------------------------------------------------------------

/// RAII exclusive lock on a run directory's `.lock` file. Held for the
/// duration of a turn-execution pass so a concurrent `investigate
/// continue` on the same run fails closed (plan.md:997). The `flock` is
/// released when the file handle drops (including process death), so a
/// crashed driver never leaves a permanently stuck run.
#[derive(Debug)]
pub struct RunLock {
    #[allow(dead_code)]
    file: File,
    run_id: String,
}

impl RunLock {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[cfg(unix)]
impl Drop for RunLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // Best-effort explicit unlock; closing the fd also releases it.
        // SAFETY: plain libc syscall on a fd we own.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Filesystem store for investigate run directories, rooted at a
/// `.libra/sessions` directory (runs live under
/// `<sessions_root>/agent-runs/<run_id>/`).
#[derive(Clone, Debug)]
pub struct InvestigateRunStore {
    sessions_root: PathBuf,
}

impl InvestigateRunStore {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
        }
    }

    pub fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    pub fn runs_root(&self) -> PathBuf {
        self.sessions_root.join(AGENT_RUNS_DIR)
    }

    pub fn run_dir(&self, run_id: &str) -> io::Result<PathBuf> {
        if !is_valid_run_id(run_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid investigate run id '{run_id}': expected only ASCII letters, \
                     digits, '-' or '_' (run `libra investigate list` for known run ids)"
                ),
            ));
        }
        Ok(self.runs_root().join(run_id))
    }

    pub fn state_path(&self, run_id: &str) -> io::Result<PathBuf> {
        Ok(self.run_dir(run_id)?.join("state.json"))
    }

    pub fn manifest_path(&self, run_id: &str) -> io::Result<PathBuf> {
        Ok(self.run_dir(run_id)?.join("manifest.json"))
    }

    pub fn findings_path(&self, run_id: &str) -> io::Result<PathBuf> {
        Ok(self.run_dir(run_id)?.join(INVESTIGATE_FINDINGS_DOC))
    }

    pub fn reviewers_dir(&self, run_id: &str) -> io::Result<PathBuf> {
        Ok(self.run_dir(run_id)?.join("reviewers"))
    }

    /// `reviewers/<name>.stdout.redacted.log`.
    pub fn investigator_stdout_log_path(&self, run_id: &str, name: &str) -> io::Result<PathBuf> {
        Ok(self
            .reviewers_dir(run_id)?
            .join(format!("{name}.stdout.redacted.log")))
    }

    /// `reviewers/<name>.stderr.redacted.log`.
    pub fn investigator_stderr_log_path(&self, run_id: &str, name: &str) -> io::Result<PathBuf> {
        Ok(self
            .reviewers_dir(run_id)?
            .join(format!("{name}.stderr.redacted.log")))
    }

    /// Create the run directory plus the initial `state.json`,
    /// `manifest.json` and an empty `findings.md`. Fails with
    /// [`io::ErrorKind::AlreadyExists`] if the run already exists.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run(
        &self,
        run_id: &str,
        topic: &str,
        agents: &[String],
        max_turns: u32,
        quorum: u32,
        starting_sha: &str,
    ) -> io::Result<InvestigateRunState> {
        let dir = self.run_dir(run_id)?;
        if dir.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "investigate run '{run_id}' already exists at {}",
                    dir.display()
                ),
            ));
        }
        fs::create_dir_all(dir.join("reviewers"))?;

        let now = utc_timestamp();
        let state = InvestigateRunState {
            schema_version: INVESTIGATE_MANIFEST_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            topic: topic.to_string(),
            kind: INVESTIGATE_RUN_KIND.to_string(),
            agents: agents.to_vec(),
            max_turns,
            quorum,
            completed_rounds: 0,
            turn: 0,
            next_agent_idx: 0,
            stances: Vec::new(),
            findings_doc: INVESTIGATE_FINDINGS_DOC.to_string(),
            starting_sha: starting_sha.to_string(),
            started_at: now.clone(),
            updated_at: now.clone(),
            pending_turn: None,
            terminal_state: None,
            cancel_requested: false,
            workspace_root: None,
        };
        let manifest = InvestigateManifest {
            schema_version: INVESTIGATE_MANIFEST_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            kind: INVESTIGATE_RUN_KIND.to_string(),
            agents: agents.to_vec(),
            starting_sha: starting_sha.to_string(),
            target_scope: topic.to_string(),
            terminal_state: None,
            created_at: now.clone(),
            updated_at: now,
            findings_oid: None,
            redaction_report: RedactionReportSummary::default(),
            manual_attach: Vec::new(),
        };
        self.write_state(&state)?;
        self.write_manifest(&manifest)?;
        fs::write(dir.join(INVESTIGATE_FINDINGS_DOC), b"")?;
        Ok(state)
    }

    /// Acquire the run's exclusive OS lock (`flock` on `<run_dir>/.lock`).
    /// Returns [`io::ErrorKind::WouldBlock`] when another process already
    /// holds it (a concurrent `investigate continue` — fail closed,
    /// plan.md:997), and [`io::ErrorKind::NotFound`] when the run does not
    /// exist.
    pub fn try_lock_run(&self, run_id: &str) -> io::Result<RunLock> {
        let dir = self.run_dir(run_id)?;
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("investigate run '{run_id}' not found (run `libra investigate list`)"),
            ));
        }
        let path = dir.join(RUN_LOCK_FILE);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&path)?;
        acquire_flock(&file, run_id)?;
        Ok(RunLock {
            file,
            run_id: run_id.to_string(),
        })
    }

    /// Whole-file overwrite of `state.json`.
    pub fn write_state(&self, state: &InvestigateRunState) -> io::Result<()> {
        let path = self.state_path(&state.run_id)?;
        let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    /// Load-modify-write one run's `state.json`, bumping `updated_at`.
    /// Returns `Ok(false)` when the run does not exist.
    pub fn update_state<F>(&self, run_id: &str, mutate: F) -> io::Result<bool>
    where
        F: FnOnce(&mut InvestigateRunState),
    {
        let Some(mut state) = self.load_state(run_id)? else {
            return Ok(false);
        };
        mutate(&mut state);
        state.updated_at = utc_timestamp();
        self.write_state(&state)?;
        Ok(true)
    }

    pub fn write_manifest(&self, manifest: &InvestigateManifest) -> io::Result<()> {
        let path = self.manifest_path(&manifest.run_id)?;
        let json = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    pub fn load_state(&self, run_id: &str) -> io::Result<Option<InvestigateRunState>> {
        read_json_opt(&self.state_path(run_id)?)
    }

    pub fn load_manifest(&self, run_id: &str) -> io::Result<Option<InvestigateManifest>> {
        read_json_opt(&self.manifest_path(run_id)?)
    }

    /// Overwrite `findings.md` (single-writer snapshot of already-redacted
    /// content — the runner is the sole producer).
    pub fn write_findings(&self, run_id: &str, content: &str) -> io::Result<()> {
        fs::write(self.findings_path(run_id)?, content.as_bytes())
    }

    pub fn read_findings(&self, run_id: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.findings_path(run_id)?) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Append one redacted block to an investigator log.
    pub fn append_investigator_log(&self, path: &Path, content: &str) -> io::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(content.as_bytes())
    }

    /// Update the manifest's terminal state + redaction summary, bumping
    /// `updated_at`. `terminal` is `None` for a paused run (stays in
    /// flight).
    /// A0-06: the `.libra` dir (git object-store root) derived from the
    /// store's `<.libra>/sessions` root.
    fn libra_dir(&self) -> io::Result<PathBuf> {
        self.sessions_root()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "investigate sessions root has no parent .libra directory",
                )
            })
    }

    /// A0-06: write `bytes` as a content-addressed git blob and enqueue it
    /// into `object_index` (tag [`AGENT_FINDINGS_OTYPE`]), returning the OID.
    /// Idempotent by content-addressing.
    pub fn objectize_bytes(&self, bytes: &[u8]) -> io::Result<String> {
        let libra_dir = self.libra_dir()?;
        let oid = crate::utils::object::write_git_object(&libra_dir, "blob", bytes)
            .map_err(|e| io::Error::other(format!("failed to write findings object: {e}")))?;
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &libra_dir,
            &oid.to_string(),
            AGENT_FINDINGS_OTYPE,
            bytes.len() as i64,
        );
        Ok(oid.to_string())
    }

    /// A0-06: objectize the run's current `findings.md`. `None` when empty.
    pub fn objectize_findings(&self, run_id: &str) -> io::Result<Option<String>> {
        let content = self.read_findings(run_id)?.unwrap_or_default();
        if content.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.objectize_bytes(content.as_bytes())?))
    }

    pub fn write_manifest_terminal(
        &self,
        run_id: &str,
        terminal: Option<InvestigateTerminalState>,
        redaction: RedactionReportSummary,
    ) -> io::Result<()> {
        let mut manifest = self.load_manifest(run_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("investigate run '{run_id}' has no manifest.json to finalize"),
            )
        })?;
        manifest.terminal_state = terminal;
        manifest.redaction_report = redaction;
        manifest.updated_at = utc_timestamp();
        // A0-06: objectize the final findings.md so `findings_oid` points at a
        // real, object_index-visible, doctor-repairable blob.
        if let Some(oid) = self.objectize_findings(run_id)? {
            manifest.findings_oid = Some(oid);
        }
        self.write_manifest(&manifest)
    }

    /// Cross-process cancel request: drop a marker file the owning driver
    /// polls. Returns `false` when the run does not exist.
    pub fn mark_cancel_requested(&self, run_id: &str) -> io::Result<bool> {
        let dir = self.run_dir(run_id)?;
        if !dir.is_dir() {
            return Ok(false);
        }
        fs::write(dir.join(CANCEL_REQUESTED_FILE), b"")?;
        Ok(true)
    }

    pub fn cancel_requested(&self, run_id: &str) -> bool {
        self.run_dir(run_id)
            .map(|dir| dir.join(CANCEL_REQUESTED_FILE).exists())
            .unwrap_or(false)
    }

    /// Mark a non-running run `cancelled` directly (used by
    /// `investigate cancel` when no live driver answers the marker).
    /// Returns `Ok(true)` when it transitioned, `Ok(false)` when already
    /// terminal, and [`io::ErrorKind::NotFound`] when it does not exist.
    pub fn mark_cancelled(&self, run_id: &str) -> io::Result<bool> {
        let mut state = self.load_state(run_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("investigate run '{run_id}' not found (run `libra investigate list`)"),
            )
        })?;
        if state.is_terminal() {
            return Ok(false);
        }
        // A cancel discards the pending resume point.
        state.pending_turn = None;
        state.terminal_state = Some(InvestigateTerminalState::Cancelled);
        state.updated_at = utc_timestamp();
        self.write_state(&state)?;
        let redaction = self
            .load_manifest(run_id)?
            .map(|m| m.redaction_report)
            .unwrap_or_default();
        self.write_manifest_terminal(run_id, Some(InvestigateTerminalState::Cancelled), redaction)?;
        Ok(true)
    }

    /// Every run, sorted in keyset order (`started_at DESC, run_id DESC`).
    /// Only investigate-kind runs are returned (the directory is shared
    /// with review). Unreadable or foreign entries are skipped.
    pub fn list_runs(&self) -> io::Result<Vec<InvestigateRunSummary>> {
        let mut runs = Vec::new();
        let entries = match fs::read_dir(self.runs_root()) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(Some(state)) =
                read_json_opt::<InvestigateRunState>(&entry.path().join("state.json"))
            else {
                continue;
            };
            if state.kind != INVESTIGATE_RUN_KIND {
                continue;
            }
            let pause_reason = state.pending_turn.as_ref().map(|p| p.reason);
            runs.push(InvestigateRunSummary {
                run_id: state.run_id,
                kind: state.kind,
                agents: state.agents,
                topic: state.topic,
                turn: state.turn,
                max_turns: state.max_turns,
                terminal_state: state.terminal_state,
                pause_reason,
                started_at: state.started_at,
                updated_at: state.updated_at,
            });
        }
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.run_id.cmp(&a.run_id))
        });
        Ok(runs)
    }

    /// One keyset page after `cursor` (exclusive), `limit` items max.
    pub fn list_runs_page(
        &self,
        cursor: Option<&InvestigateRunCursor>,
        limit: usize,
    ) -> io::Result<InvestigateRunPage> {
        let all = self.list_runs()?;
        let after = |run: &InvestigateRunSummary| match cursor {
            None => true,
            Some(cursor) => {
                (run.started_at.as_str(), run.run_id.as_str())
                    < (cursor.started_at.as_str(), cursor.run_id.as_str())
            }
        };
        let mut remaining = all.into_iter().filter(after);
        let items: Vec<InvestigateRunSummary> = remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        let next_cursor = if has_more {
            items.last().map(|run| InvestigateRunCursor {
                started_at: run.started_at.clone(),
                run_id: run.run_id.clone(),
            })
        } else {
            None
        };
        Ok(InvestigateRunPage {
            items,
            next_cursor,
            has_more,
        })
    }

    /// Remove one run directory. `Ok(false)` when it did not exist.
    pub fn clean_run(&self, run_id: &str) -> io::Result<bool> {
        let dir = self.run_dir(run_id)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Remove every investigate-kind run directory; returns how many were
    /// removed. Review runs sharing the directory are left untouched.
    pub fn clean_all(&self) -> io::Result<usize> {
        let entries = match fs::read_dir(self.runs_root()) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut removed = 0usize;
        for entry in entries {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            // Only sweep investigate runs; leave review runs alone.
            match read_json_opt::<InvestigateRunState>(&path.join("state.json")) {
                Ok(Some(state)) if state.kind == INVESTIGATE_RUN_KIND => {
                    fs::remove_dir_all(&path)?;
                    removed += 1;
                }
                _ => continue,
            }
        }
        Ok(removed)
    }
}

/// Try to acquire an exclusive, non-blocking `flock` on `file`.
#[cfg(unix)]
fn acquire_flock(file: &File, run_id: &str) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: plain libc syscall on a fd we own.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "investigate run '{run_id}' is already being driven by another process; \
                 wait for it to finish or check `libra investigate show {run_id}`"
            ),
        )),
        _ => Err(error),
    }
}

/// Non-unix fallback: no OS advisory lock available, so the lock is a
/// best-effort no-op (single-writer is still enforced by the run's serial
/// turn loop within a process).
#[cfg(not(unix))]
fn acquire_flock(_file: &File, _run_id: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn store() -> (tempfile::TempDir, InvestigateRunStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = InvestigateRunStore::new(dir.path().join(".libra").join("sessions"));
        (dir, store)
    }

    /// E8-libra manifest contract: exactly the 12 keys, no more, no less,
    /// with `kind = "investigate"` (`agent.md:876` / `agent.md:1321`).
    #[test]
    fn manifest_serializes_exactly_the_e8_key_set_with_investigate_kind() {
        let manifest = InvestigateManifest {
            schema_version: INVESTIGATE_MANIFEST_SCHEMA_VERSION,
            run_id: "r1".into(),
            kind: INVESTIGATE_RUN_KIND.into(),
            agents: vec!["codex".into()],
            starting_sha: "abc123".into(),
            target_scope: "why is startup slow".into(),
            terminal_state: Some(InvestigateTerminalState::Quorum),
            created_at: utc_timestamp(),
            updated_at: utc_timestamp(),
            findings_oid: None,
            redaction_report: RedactionReportSummary::default(),
            manual_attach: Vec::new(),
        };
        let value = serde_json::to_value(&manifest).expect("manifest to json");
        let keys: BTreeSet<String> = value
            .as_object()
            .expect("manifest is a json object")
            .keys()
            .cloned()
            .collect();
        let expected: BTreeSet<String> = [
            "schema_version",
            "run_id",
            "kind",
            "agents",
            "starting_sha",
            "target_scope",
            "terminal_state",
            "created_at",
            "updated_at",
            "findings_oid",
            "redaction_report",
            "manual_attach",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(
            keys, expected,
            "manifest.json must carry exactly the E8 keys"
        );
        assert_eq!(value["kind"], "investigate");
        assert_eq!(value["manual_attach"], serde_json::json!([]));
        assert_eq!(value["findings_oid"], serde_json::Value::Null);
    }

    /// state.json round-trips and carries every E8-entire round-robin
    /// field (plan.md:995 / agent.md:869).
    #[test]
    fn state_round_trips_with_all_round_robin_fields() {
        let (_dir, store) = store();
        let agents = vec!["codex".to_string(), "opencode".to_string()];
        let state = store
            .create_run("run-a", "why slow", &agents, 6, 2, "sha-1")
            .expect("create");
        assert!(!state.is_terminal());
        assert_eq!(state.turn, 0);
        assert_eq!(state.next_agent_idx, 0);
        assert_eq!(state.findings_doc, INVESTIGATE_FINDINGS_DOC);

        // The serialized object must contain every named field.
        let value = serde_json::to_value(&state).expect("state to json");
        let object = value.as_object().expect("state is object");
        for key in [
            "run_id",
            "topic",
            "agents",
            "max_turns",
            "quorum",
            "completed_rounds",
            "turn",
            "next_agent_idx",
            "stances",
            "findings_doc",
            "starting_sha",
            "started_at",
            "updated_at",
            "pending_turn",
        ] {
            assert!(object.contains_key(key), "state.json missing `{key}`");
        }

        // Double-create fails loudly.
        let err = store
            .create_run("run-a", "x", &agents, 6, 2, "sha-1")
            .expect_err("duplicate create");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        // Round-trip a paused mutation.
        store
            .update_state("run-a", |state| {
                state.stances.push(StanceEntry {
                    turn: 1,
                    agent_idx: 0,
                    slug: "codex".into(),
                    disposition: StanceDisposition::Continuing,
                    summary: "looking".into(),
                    exit_code: Some(0),
                    stdout_truncated: false,
                });
                state.turn = 1;
                state.next_agent_idx = 1;
                state.pending_turn = Some(PendingTurn {
                    turn: 2,
                    agent_idx: 1,
                    slug: "opencode".into(),
                    reason: PauseReason::Stalled,
                    detail: None,
                });
            })
            .expect("update");
        let reloaded = store.load_state("run-a").expect("load").expect("state");
        assert!(reloaded.is_paused());
        assert_eq!(reloaded.turn, 1);
        assert_eq!(
            reloaded.pending_turn.as_ref().unwrap().reason,
            PauseReason::Stalled
        );
    }

    #[test]
    fn concluding_agent_count_is_distinct_by_slug() {
        let (_dir, store) = store();
        store
            .create_run("run-q", "t", &["a".into(), "b".into()], 10, 2, "sha")
            .expect("create");
        store
            .update_state("run-q", |state| {
                let mk = |turn, idx, slug: &str, d| StanceEntry {
                    turn,
                    agent_idx: idx,
                    slug: slug.into(),
                    disposition: d,
                    summary: String::new(),
                    exit_code: Some(0),
                    stdout_truncated: false,
                };
                state
                    .stances
                    .push(mk(1, 0, "a", StanceDisposition::Concluding));
                // Same agent concluding twice → still counts once.
                state
                    .stances
                    .push(mk(3, 0, "a", StanceDisposition::Concluding));
                state
                    .stances
                    .push(mk(2, 1, "b", StanceDisposition::Continuing));
            })
            .expect("update");
        let state = store.load_state("run-q").expect("load").expect("state");
        assert_eq!(state.concluding_agent_count(), 1);
    }

    #[test]
    fn stance_disposition_classification_is_conservative() {
        assert_eq!(
            classify_stance_disposition("I conclude the leak is in cache.rs"),
            StanceDisposition::Concluding
        );
        assert_eq!(
            classify_stance_disposition("STANCE: concluding\nroot cause found"),
            StanceDisposition::Concluding
        );
        assert_eq!(
            classify_stance_disposition("still investigating, need another pass"),
            StanceDisposition::Continuing
        );
        assert_eq!(
            classify_stance_disposition(""),
            StanceDisposition::Continuing
        );
    }

    #[test]
    fn list_is_keyset_ordered_and_paginates_and_ignores_review_runs() {
        let (_dir, store) = store();
        for (run_id, started_at) in [
            ("run-a", "2026-07-01T00:00:00.000000Z"),
            ("run-b", "2026-07-02T00:00:00.000000Z"),
            ("run-c", "2026-07-02T00:00:00.000000Z"),
            ("run-d", "2026-07-03T00:00:00.000000Z"),
        ] {
            let mut state = store
                .create_run(run_id, "t", &["codex".into()], 4, 1, "sha")
                .expect("create");
            state.started_at = started_at.to_string();
            state.updated_at = started_at.to_string();
            store.write_state(&state).expect("write");
        }
        // A foreign (review-kind) directory must be ignored by list.
        let review_dir = store.runs_root().join("review-run");
        fs::create_dir_all(&review_dir).expect("review dir");
        fs::write(
            review_dir.join("state.json"),
            serde_json::json!({
                "schema_version": 1, "run_id": "review-run", "kind": "review",
                "agents": [], "starting_sha": "s", "target_scope": "sc",
                "terminal_state": null, "created_at": "2026-07-09T00:00:00.000000Z",
                "updated_at": "2026-07-09T00:00:00.000000Z"
            })
            .to_string(),
        )
        .expect("write review state");

        let all = store.list_runs().expect("list");
        let ids: Vec<&str> = all.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-d", "run-c", "run-b", "run-a"]);

        let page1 = store.list_runs_page(None, 3).expect("page1");
        assert_eq!(page1.items.len(), 3);
        assert!(page1.has_more);
        let cursor = page1.next_cursor.clone().expect("cursor");
        assert_eq!(cursor.run_id, "run-b");
        let page2 = store.list_runs_page(Some(&cursor), 3).expect("page2");
        assert_eq!(page2.items.len(), 1);
        assert!(!page2.has_more);
        let mut walked: Vec<String> = page1
            .items
            .iter()
            .chain(page2.items.iter())
            .map(|r| r.run_id.clone())
            .collect();
        walked.sort();
        assert_eq!(walked, ["run-a", "run-b", "run-c", "run-d"]);
    }

    #[test]
    fn cancel_mark_and_clean_only_touch_investigate_runs() {
        let (_dir, store) = store();
        store
            .create_run("run-x", "t", &["codex".into()], 4, 1, "sha")
            .expect("create");
        assert!(!store.cancel_requested("run-x"));
        assert!(store.mark_cancel_requested("run-x").expect("mark"));
        assert!(store.cancel_requested("run-x"));

        assert!(store.mark_cancelled("run-x").expect("cancel"));
        let state = store.load_state("run-x").expect("load").expect("state");
        assert_eq!(
            state.terminal_state,
            Some(InvestigateTerminalState::Cancelled)
        );
        assert!(!store.mark_cancelled("run-x").expect("cancel again"));

        assert!(store.clean_run("run-x").expect("clean"));
        assert!(!store.clean_run("run-x").expect("clean missing"));

        store
            .create_run("run-y", "t", &["codex".into()], 4, 1, "sha")
            .expect("create");
        // A review-kind dir must survive clean_all.
        let review_dir = store.runs_root().join("keep-review");
        fs::create_dir_all(&review_dir).expect("review dir");
        fs::write(
            review_dir.join("state.json"),
            serde_json::json!({"kind": "review"}).to_string(),
        )
        .expect("write");
        assert_eq!(store.clean_all().expect("clean all"), 1);
        assert!(review_dir.exists(), "review runs must not be swept");
    }

    #[test]
    fn run_id_validation_blocks_traversal() {
        let (_dir, store) = store();
        for bad in ["", "../evil", "a/b", "a\\b", "..", "run id"] {
            assert!(
                store.run_dir(bad).is_err(),
                "run id '{bad}' must be rejected"
            );
        }
        assert!(store.run_dir("0e6f0a1c-run_1").is_ok());
    }

    /// The run lock is exclusive: a second acquisition while the first is
    /// held fails closed with `WouldBlock` (plan.md:997), and releasing
    /// the first makes the run lockable again.
    #[cfg(unix)]
    #[test]
    fn run_lock_is_exclusive_and_fails_closed() {
        let (_dir, store) = store();
        store
            .create_run("run-lock", "t", &["codex".into()], 4, 1, "sha")
            .expect("create");
        let guard = store.try_lock_run("run-lock").expect("first lock");
        let err = store
            .try_lock_run("run-lock")
            .expect_err("second lock must fail closed");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(guard);
        // Released — lockable again.
        let _again = store
            .try_lock_run("run-lock")
            .expect("relock after release");

        // Locking a missing run is NotFound.
        assert_eq!(
            store.try_lock_run("nope").expect_err("missing run").kind(),
            io::ErrorKind::NotFound
        );
    }
}
