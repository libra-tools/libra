//! Review run directory store (`.libra/sessions/agent-runs/<run_id>/`).
//!
//! Persists the E8-libra run wire (`docs/development/tracing/agent.md`
//! E8-libra + 落地执行补充规格 §5): per run one directory containing
//! `state.json`, `manifest.json`, `findings.md` and
//! `reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`.
//!
//! Modeled on [`crate::internal::ai::agent_run::event_store::AgentRunEventStore`]:
//! the store is stateless beyond its `sessions_root`, every write is a
//! whole-file JSON overwrite (`fs::write` of `serde_json::to_vec_pretty`)
//! and reads tolerate missing files. A run directory is written by exactly
//! one producer (the runner driving that run) plus the small cross-process
//! cancel/clean surface below; concurrent multi-writer coordination is out
//! of scope for AG-22.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::internal::ai::observed_agents::RedactionReport;

/// `object_index` o_type tag for objectized review/investigate findings +
/// manual attachments (A0-06). A distinguished tag (parallel to
/// `agent_transcript`) so `libra agent doctor` and retention can enumerate
/// findings artifacts; cloud sync does not filter by o_type either way.
pub const AGENT_FINDINGS_OTYPE: &str = "agent_findings";

/// `manifest.json` schema version (E8-libra manifest contract).
pub const REVIEW_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The `kind` value review runs stamp into `state.json` / `manifest.json`
/// (`review|investigate` per E8-libra; AG-23 owns `investigate`).
pub const REVIEW_RUN_KIND: &str = "review";

/// Directory under the sessions root holding one subdirectory per run.
const AGENT_RUNS_DIR: &str = "agent-runs";
/// Cross-process cancel-request marker file inside a run directory.
const CANCEL_REQUESTED_FILE: &str = "cancel.requested";

// ---------------------------------------------------------------------------
// Terminal states
// ---------------------------------------------------------------------------

/// The five terminal states every review run must end in exactly one of
/// (plan.md Task A7 验收; `agent.md` §6 `agent.review.run`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTerminalState {
    /// Every reviewer completed with exit status 0.
    Success,
    /// Infrastructure failure (workspace/store), or no reviewer succeeded
    /// and none timed out.
    Error,
    /// The run was cancelled (`review cancel` / SIGINT / SIGTERM — one
    /// shared cleanup path).
    Cancelled,
    /// No reviewer succeeded and at least one hit its deadline.
    Timeout,
    /// Some — but not all — reviewers succeeded.
    Partial,
}

impl ReviewTerminalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Partial => "partial",
        }
    }
}

impl std::fmt::Display for ReviewTerminalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-reviewer outcome recorded in `state.json` and aggregated into the
/// run's terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerOutcome {
    /// Exit status 0.
    Ok,
    /// Spawn failure or non-zero exit status.
    Failed,
    /// Killed at the per-reviewer deadline.
    TimedOut,
    /// Killed by the run-level cancel path.
    Cancelled,
}

/// Deterministic terminal-state aggregation (unit-tested truth table):
///
/// - run-level cancel always wins → [`ReviewTerminalState::Cancelled`];
/// - no reviewer ever produced an outcome (infrastructure failed before
///   any reviewer ran) → [`ReviewTerminalState::Error`];
/// - all `Ok` → [`ReviewTerminalState::Success`];
/// - some (but not all) `Ok` → [`ReviewTerminalState::Partial`];
/// - none `Ok`, at least one `TimedOut` → [`ReviewTerminalState::Timeout`];
/// - none `Ok`, none `TimedOut` → [`ReviewTerminalState::Error`].
pub fn aggregate_terminal_state(
    cancelled: bool,
    outcomes: &[ReviewerOutcome],
) -> ReviewTerminalState {
    if cancelled {
        return ReviewTerminalState::Cancelled;
    }
    if outcomes.is_empty() {
        return ReviewTerminalState::Error;
    }
    let ok = outcomes
        .iter()
        .filter(|o| matches!(o, ReviewerOutcome::Ok))
        .count();
    if ok == outcomes.len() {
        ReviewTerminalState::Success
    } else if ok > 0 {
        ReviewTerminalState::Partial
    } else if outcomes
        .iter()
        .any(|o| matches!(o, ReviewerOutcome::TimedOut))
    {
        ReviewTerminalState::Timeout
    } else {
        ReviewTerminalState::Error
    }
}

// ---------------------------------------------------------------------------
// Manifest (E8 exact key set)
// ---------------------------------------------------------------------------

/// Deserializable summary of a redaction pass. The full
/// [`RedactionReport`] is `Serialize`-only (its match list carries rule
/// offsets the manifest does not need), so the manifest stores this
/// round-trippable aggregate instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReportSummary {
    /// Total redaction rule matches across all persisted reviewer text.
    pub matches: usize,
    pub bytes_scanned: usize,
    pub bytes_redacted: usize,
}

impl RedactionReportSummary {
    pub fn absorb(&mut self, report: &RedactionReport) {
        self.matches += report.matches.len();
        self.bytes_scanned += report.bytes_scanned;
        self.bytes_redacted += report.bytes_redacted;
    }

    pub fn merge(&mut self, other: &RedactionReportSummary) {
        self.matches += other.matches;
        self.bytes_scanned += other.bytes_scanned;
        self.bytes_redacted += other.bytes_redacted;
    }
}

/// `manifest.json` — **exactly** the E8-libra key set
/// (`agent.md:876` / `agent.md:1321`): `schema_version`, `run_id`,
/// `kind`, `agents`, `starting_sha`, `target_scope`, `terminal_state`,
/// `created_at`, `updated_at`, `findings_oid`, `redaction_report`,
/// `manual_attach`. The key-set exactness is pinned by a unit test below;
/// do not add or rename fields without updating the E8 spec first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewManifest {
    pub schema_version: u32,
    pub run_id: String,
    /// `"review"` (`REVIEW_RUN_KIND`); `"investigate"` is AG-23's.
    pub kind: String,
    /// Reviewer slugs in request order.
    pub agents: Vec<String>,
    /// The commit the reviewed worktree started from.
    pub starting_sha: String,
    /// Human-readable review scope (e.g. `--since <rev>` resolution).
    pub target_scope: String,
    /// `null` while the run is in flight; exactly one of the five
    /// terminal states once it ends.
    pub terminal_state: Option<ReviewTerminalState>,
    /// RFC 3339 UTC, microsecond precision (fixed width, so
    /// lexicographic order equals chronological order — the keyset
    /// pagination contract relies on this).
    pub created_at: String,
    pub updated_at: String,
    /// OID of `findings.md` in the object store. `null` at create time and
    /// for an empty findings set; populated at `finalize_run` (A0-06) with a
    /// content-addressed, `object_index`-visible, doctor-repairable blob.
    pub findings_oid: Option<String>,
    pub redaction_report: RedactionReportSummary,
    /// Manual attachments (A0-06): each entry is
    /// `{oid, path, provenance:"manual", size, attached_at}` for an external
    /// transcript/findings/context file attached via `libra review attach`.
    /// Empty until the first attach; the attached bytes are redacted and
    /// object_index-tagged like findings.
    pub manual_attach: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// State (engine-internal, richer than the manifest)
// ---------------------------------------------------------------------------

/// Per-reviewer row in `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerStateEntry {
    pub slug: String,
    /// `null` until the reviewer finishes.
    pub outcome: Option<ReviewerOutcome>,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
    /// Redacted one-line spawn/launch error, when the reviewer never ran.
    #[serde(default)]
    pub launch_error: Option<String>,
    /// The spawned reviewer's process id, written through at spawn time
    /// so an orphaned-run cancel (runner crashed or was killed) can
    /// still verify and release the process. `null` before spawn and in
    /// terminal snapshots (finalize rewrites the rows without live
    /// pids).
    #[serde(default)]
    pub pid: Option<u32>,
    /// The reviewer's process-group id (== `pid`: reviewers are spawned
    /// as their own group leaders). The orphan cancel path group-kills
    /// this, taking reviewer descendants down too — but only after the
    /// `proc_start_ticks` provenance check verifies the pid still names
    /// OUR process incarnation.
    #[serde(default)]
    pub pgid: Option<u32>,
    /// Process provenance: the spawned reviewer's kernel start time
    /// (`/proc/<pid>/stat` field 22, clock ticks; Linux only). Pids are
    /// reused, so the orphaned-run cancel refuses to kill a recorded
    /// pgid whose current start time does not match this value (or when
    /// either side is unavailable).
    #[serde(default)]
    pub proc_start_ticks: Option<u64>,
}

/// `state.json` — the run's E8 state record (terminal state, timestamps,
/// agents, scope). Review has no round-robin fields; those are
/// investigate's (AG-23).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRunState {
    pub schema_version: u32,
    pub run_id: String,
    pub kind: String,
    pub agents: Vec<ReviewerStateEntry>,
    pub starting_sha: String,
    pub target_scope: String,
    /// `null` while running; exactly one of the five states at the end.
    pub terminal_state: Option<ReviewTerminalState>,
    pub created_at: String,
    pub updated_at: String,
    /// Set when a cross-process cancel request was observed.
    #[serde(default)]
    pub cancel_requested: bool,
    /// Root of the run's materialized isolated workspace, recorded
    /// before reviewers spawn so an orphaned-run cancel can release the
    /// directory even when no runner is alive. Stale after a normal
    /// finalize (the runner already removed the workspace) — the orphan
    /// path checks existence before acting.
    #[serde(default)]
    pub workspace_root: Option<String>,
}

impl ReviewRunState {
    pub fn is_terminal(&self) -> bool {
        self.terminal_state.is_some()
    }
}

// ---------------------------------------------------------------------------
// Listing / keyset pagination
// ---------------------------------------------------------------------------

/// One row of `review list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRunSummary {
    pub run_id: String,
    pub kind: String,
    pub agents: Vec<String>,
    pub target_scope: String,
    pub terminal_state: Option<ReviewTerminalState>,
    pub created_at: String,
    pub updated_at: String,
}

/// Keyset cursor for `review list` pagination: strictly-after position in
/// (`created_at DESC`, `run_id DESC`) order (`agent.md` 强制补强项 #5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRunCursor {
    pub created_at: String,
    pub run_id: String,
}

/// One keyset page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewRunPage {
    pub items: Vec<ReviewRunSummary>,
    pub next_cursor: Option<ReviewRunCursor>,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Filesystem store for review run directories, rooted at a
/// `.libra/sessions` directory (runs live under
/// `<sessions_root>/agent-runs/<run_id>/`).
#[derive(Clone, Debug)]
pub struct ReviewRunStore {
    sessions_root: PathBuf,
}

impl ReviewRunStore {
    /// Construct a store rooted at a `.libra/sessions` directory.
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
        }
    }

    pub fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    /// `<sessions_root>/agent-runs`.
    pub fn runs_root(&self) -> PathBuf {
        self.sessions_root.join(AGENT_RUNS_DIR)
    }

    /// `<sessions_root>/agent-runs/<run_id>` (the `run_id` is validated
    /// against path traversal — see [`is_valid_run_id`]).
    pub fn run_dir(&self, run_id: &str) -> io::Result<PathBuf> {
        if !is_valid_run_id(run_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid review run id '{run_id}': expected only ASCII letters, digits, \
                     '-' or '_' (run `libra review list` for known run ids)"
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
        Ok(self.run_dir(run_id)?.join("findings.md"))
    }

    pub fn reviewers_dir(&self, run_id: &str) -> io::Result<PathBuf> {
        Ok(self.run_dir(run_id)?.join("reviewers"))
    }

    /// `reviewers/<name>.stdout.redacted.log` (the caller supplies a
    /// filesystem-safe reviewer name — see [`sanitize_reviewer_name`]).
    pub fn reviewer_stdout_log_path(&self, run_id: &str, name: &str) -> io::Result<PathBuf> {
        Ok(self
            .reviewers_dir(run_id)?
            .join(format!("{name}.stdout.redacted.log")))
    }

    /// `reviewers/<name>.stderr.redacted.log`.
    pub fn reviewer_stderr_log_path(&self, run_id: &str, name: &str) -> io::Result<PathBuf> {
        Ok(self
            .reviewers_dir(run_id)?
            .join(format!("{name}.stderr.redacted.log")))
    }

    /// Create the run directory (`reviewers/` included) plus the initial
    /// `state.json`, `manifest.json` and an empty `findings.md`. Fails
    /// with [`io::ErrorKind::AlreadyExists`] if the run already exists.
    pub fn create_run(
        &self,
        run_id: &str,
        agents: &[String],
        starting_sha: &str,
        target_scope: &str,
    ) -> io::Result<ReviewRunState> {
        let dir = self.run_dir(run_id)?;
        if dir.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("review run '{run_id}' already exists at {}", dir.display()),
            ));
        }
        fs::create_dir_all(dir.join("reviewers"))?;

        let now = utc_timestamp();
        let state = ReviewRunState {
            schema_version: REVIEW_MANIFEST_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            kind: REVIEW_RUN_KIND.to_string(),
            agents: agents
                .iter()
                .map(|slug| ReviewerStateEntry {
                    slug: slug.clone(),
                    outcome: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    launch_error: None,
                    pid: None,
                    pgid: None,
                    proc_start_ticks: None,
                })
                .collect(),
            starting_sha: starting_sha.to_string(),
            target_scope: target_scope.to_string(),
            terminal_state: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            cancel_requested: false,
            workspace_root: None,
        };
        let manifest = ReviewManifest {
            schema_version: REVIEW_MANIFEST_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            kind: REVIEW_RUN_KIND.to_string(),
            agents: agents.to_vec(),
            starting_sha: starting_sha.to_string(),
            target_scope: target_scope.to_string(),
            terminal_state: None,
            created_at: now.clone(),
            updated_at: now,
            findings_oid: None,
            redaction_report: RedactionReportSummary::default(),
            manual_attach: Vec::new(),
        };
        self.write_state(&state)?;
        self.write_manifest(&manifest)?;
        fs::write(dir.join("findings.md"), b"")?;
        Ok(state)
    }

    /// Whole-file overwrite of `state.json` (snapshot-writer pattern —
    /// the latest write supersedes earlier ones).
    pub fn write_state(&self, state: &ReviewRunState) -> io::Result<()> {
        let path = self.state_path(&state.run_id)?;
        let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    /// Load-modify-write one run's `state.json`, bumping `updated_at`.
    /// Returns `Ok(false)` when the run does not exist. Single-writer
    /// contract: during a run only the serial sink (spawn write-through)
    /// and the runner's sequential phases call this — never two writers
    /// concurrently.
    pub fn update_state<F>(&self, run_id: &str, mutate: F) -> io::Result<bool>
    where
        F: FnOnce(&mut ReviewRunState),
    {
        let Some(mut state) = self.load_state(run_id)? else {
            return Ok(false);
        };
        mutate(&mut state);
        state.updated_at = utc_timestamp();
        self.write_state(&state)?;
        Ok(true)
    }

    /// Whole-file overwrite of `manifest.json`.
    pub fn write_manifest(&self, manifest: &ReviewManifest) -> io::Result<()> {
        let path = self.manifest_path(&manifest.run_id)?;
        let json = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    /// Load `state.json`; `Ok(None)` when the run does not exist.
    pub fn load_state(&self, run_id: &str) -> io::Result<Option<ReviewRunState>> {
        read_json_opt(&self.state_path(run_id)?)
    }

    /// Load `manifest.json`; `Ok(None)` when the run does not exist.
    pub fn load_manifest(&self, run_id: &str) -> io::Result<Option<ReviewManifest>> {
        read_json_opt(&self.manifest_path(run_id)?)
    }

    /// Overwrite `findings.md` (already-redacted content only — the
    /// runner is the sole producer and passes text through the sink's
    /// redaction pipeline first).
    pub fn write_findings(&self, run_id: &str, content: &str) -> io::Result<()> {
        fs::write(self.findings_path(run_id)?, content.as_bytes())
    }

    /// Read `findings.md`; `Ok(None)` when the run does not exist.
    pub fn read_findings(&self, run_id: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.findings_path(run_id)?) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Append one redacted block to a reviewer log (serial-sink writer
    /// only).
    pub fn append_reviewer_log(&self, path: &Path, content: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(content.as_bytes())
    }

    /// A0-06: the `.libra` dir (git object-store root) derived from the
    /// store's `<.libra>/sessions` root.
    fn libra_dir(&self) -> io::Result<PathBuf> {
        self.sessions_root
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "review sessions root has no parent .libra directory",
                )
            })
    }

    /// A0-06: write `bytes` as a content-addressed git blob and enqueue it
    /// into `object_index` (tag [`AGENT_FINDINGS_OTYPE`]), returning the OID.
    /// Content-addressed, so re-objectizing identical bytes is idempotent.
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

    /// A0-06: objectize the run's current `findings.md` into the object store
    /// and `object_index`. Returns the blob OID, or `None` when findings are
    /// empty (nothing to objectize). The bytes are already redacted before
    /// they reach `findings.md`, so objectizing them introduces no new leak.
    pub fn objectize_findings(&self, run_id: &str) -> io::Result<Option<String>> {
        let content = self.read_findings(run_id)?.unwrap_or_default();
        if content.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.objectize_bytes(content.as_bytes())?))
    }

    /// Stamp the run terminal: updates `state.json` (outcome rows +
    /// terminal state) and `manifest.json` (terminal state + redaction
    /// summary + A0-06 `findings_oid`), bumping `updated_at` on both.
    pub fn finalize_run(
        &self,
        run_id: &str,
        terminal: ReviewTerminalState,
        agents: &[ReviewerStateEntry],
        redaction: RedactionReportSummary,
    ) -> io::Result<()> {
        let now = utc_timestamp();
        let mut state = self.load_state(run_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("review run '{run_id}' has no state.json to finalize"),
            )
        })?;
        state.agents = agents.to_vec();
        state.terminal_state = Some(terminal);
        state.updated_at = now.clone();
        self.write_state(&state)?;

        let mut manifest = self.load_manifest(run_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("review run '{run_id}' has no manifest.json to finalize"),
            )
        })?;
        manifest.terminal_state = Some(terminal);
        manifest.redaction_report = redaction;
        manifest.updated_at = now;
        // A0-06: objectize the final findings.md into the object store so the
        // manifest's `findings_oid` points at a real, object_index-visible,
        // doctor-repairable blob instead of a placeholder null.
        if let Some(oid) = self.objectize_findings(run_id)? {
            manifest.findings_oid = Some(oid);
        }
        self.write_manifest(&manifest)
    }

    /// Cross-process cancel request: drop a marker file the owning
    /// runner polls. Returns `false` when the run does not exist.
    pub fn mark_cancel_requested(&self, run_id: &str) -> io::Result<bool> {
        let dir = self.run_dir(run_id)?;
        if !dir.is_dir() {
            return Ok(false);
        }
        fs::write(dir.join(CANCEL_REQUESTED_FILE), b"")?;
        Ok(true)
    }

    /// Whether a cross-process cancel request marker exists.
    pub fn cancel_requested(&self, run_id: &str) -> bool {
        self.run_dir(run_id)
            .map(|dir| dir.join(CANCEL_REQUESTED_FILE).exists())
            .unwrap_or(false)
    }

    /// Mark a non-running (orphaned) run `cancelled` directly in
    /// `state.json` + `manifest.json`. Returns `Ok(true)` when the run
    /// transitioned, `Ok(false)` when it was already terminal, and
    /// [`io::ErrorKind::NotFound`] when it does not exist. The in-process
    /// path for live runs is [`super::runner::ReviewCancelHandle`]; both
    /// converge on the same terminal bookkeeping.
    pub fn mark_cancelled(&self, run_id: &str) -> io::Result<bool> {
        let state = self.load_state(run_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("review run '{run_id}' not found (run `libra review list`)"),
            )
        })?;
        if state.is_terminal() {
            return Ok(false);
        }
        let mut agents = state.agents.clone();
        for entry in &mut agents {
            if entry.outcome.is_none() {
                entry.outcome = Some(ReviewerOutcome::Cancelled);
            }
        }
        let redaction = self
            .load_manifest(run_id)?
            .map(|m| m.redaction_report)
            .unwrap_or_default();
        self.finalize_run(run_id, ReviewTerminalState::Cancelled, &agents, redaction)?;
        Ok(true)
    }

    /// Every run, sorted in keyset order (`created_at DESC, run_id DESC`)
    /// — the enumeration order the CLI pagination contract requires.
    /// Unreadable or foreign entries under `agent-runs/` are skipped, not
    /// fatal (tolerant-read pattern, like `list_all_snapshots`).
    pub fn list_runs(&self) -> io::Result<Vec<ReviewRunSummary>> {
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
            let Ok(Some(state)) = read_json_opt::<ReviewRunState>(&entry.path().join("state.json"))
            else {
                continue;
            };
            runs.push(ReviewRunSummary {
                run_id: state.run_id,
                kind: state.kind,
                agents: state.agents.into_iter().map(|a| a.slug).collect(),
                target_scope: state.target_scope,
                terminal_state: state.terminal_state,
                created_at: state.created_at,
                updated_at: state.updated_at,
            });
        }
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.run_id.cmp(&a.run_id))
        });
        Ok(runs)
    }

    /// One keyset page after `cursor` (exclusive), `limit` items max, in
    /// (`created_at DESC, run_id DESC`) order. The cursor is stable
    /// across inserts: newly created runs sort strictly before an
    /// existing cursor position and cannot duplicate rows into a later
    /// page.
    pub fn list_runs_page(
        &self,
        cursor: Option<&ReviewRunCursor>,
        limit: usize,
    ) -> io::Result<ReviewRunPage> {
        let all = self.list_runs()?;
        let after = |run: &ReviewRunSummary| match cursor {
            None => true,
            Some(cursor) => {
                // Strictly after the cursor in DESC order == strictly
                // smaller (created_at, run_id) tuple.
                (run.created_at.as_str(), run.run_id.as_str())
                    < (cursor.created_at.as_str(), cursor.run_id.as_str())
            }
        };
        let mut remaining = all.into_iter().filter(after);
        let items: Vec<ReviewRunSummary> = remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        let next_cursor = if has_more {
            items.last().map(|run| ReviewRunCursor {
                created_at: run.created_at.clone(),
                run_id: run.run_id.clone(),
            })
        } else {
            None
        };
        Ok(ReviewRunPage {
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

    /// Remove every run directory; returns how many were removed.
    pub fn clean_all(&self) -> io::Result<usize> {
        let entries = match fs::read_dir(self.runs_root()) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut removed = 0usize;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Run ids are uuid-shaped in production; the guard exists so a
/// CLI-supplied run id can never escape `agent-runs/` (path traversal).
pub fn is_valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id.len() <= 128
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Filesystem-safe reviewer log name: lowercased, every char outside
/// `[a-z0-9_-]` mapped to `-`, empty input mapped to `reviewer`. `.` is
/// deliberately excluded so `..` can never appear.
pub fn sanitize_reviewer_name(slug: &str) -> String {
    let name: String = slug
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "reviewer".to_string()
    } else {
        name
    }
}

/// RFC 3339 UTC with fixed microsecond precision — fixed width, so
/// lexicographic string order equals chronological order (the keyset
/// contract).
pub(crate) fn utc_timestamp() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub(crate) fn read_json_opt<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            Ok(Some(value))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn store() -> (tempfile::TempDir, ReviewRunStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReviewRunStore::new(dir.path().join(".libra").join("sessions"));
        (dir, store)
    }

    /// E8-libra manifest contract: exactly these keys, no more, no less
    /// (`agent.md:876` / `agent.md:1321`).
    #[test]
    fn manifest_serializes_exactly_the_e8_key_set() {
        let manifest = ReviewManifest {
            schema_version: REVIEW_MANIFEST_SCHEMA_VERSION,
            run_id: "r1".into(),
            kind: REVIEW_RUN_KIND.into(),
            agents: vec!["codex".into()],
            starting_sha: "abc123".into(),
            target_scope: "HEAD~1..HEAD".into(),
            terminal_state: Some(ReviewTerminalState::Success),
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
        // A create-time / attachment-free manifest serializes findings_oid as
        // null and manual_attach as []; A0-06 populates them at finalize /
        // attach without changing the 12-key set (proven in the workflow
        // tests). This case pins the empty-state serialization only.
        assert_eq!(value["manual_attach"], serde_json::json!([]));
        assert_eq!(value["findings_oid"], serde_json::Value::Null);
    }

    #[test]
    fn terminal_state_aggregation_truth_table() {
        use ReviewerOutcome::*;
        // Cancel wins over everything.
        assert_eq!(
            aggregate_terminal_state(true, &[Ok, Ok]),
            ReviewTerminalState::Cancelled
        );
        assert_eq!(
            aggregate_terminal_state(true, &[]),
            ReviewTerminalState::Cancelled
        );
        // Nothing ever ran → infrastructure error.
        assert_eq!(
            aggregate_terminal_state(false, &[]),
            ReviewTerminalState::Error
        );
        // All ok → success.
        assert_eq!(
            aggregate_terminal_state(false, &[Ok, Ok]),
            ReviewTerminalState::Success
        );
        // Mixed → partial.
        assert_eq!(
            aggregate_terminal_state(false, &[Ok, Failed]),
            ReviewTerminalState::Partial
        );
        assert_eq!(
            aggregate_terminal_state(false, &[Ok, TimedOut]),
            ReviewTerminalState::Partial
        );
        // None ok, any timed out → timeout.
        assert_eq!(
            aggregate_terminal_state(false, &[Failed, TimedOut]),
            ReviewTerminalState::Timeout
        );
        // None ok, none timed out → error.
        assert_eq!(
            aggregate_terminal_state(false, &[Failed, Failed]),
            ReviewTerminalState::Error
        );
        assert_eq!(
            aggregate_terminal_state(false, &[Cancelled]),
            ReviewTerminalState::Error
        );
    }

    #[test]
    fn create_load_finalize_roundtrip() {
        let (_dir, store) = store();
        let agents = vec!["codex".to_string(), "opencode".to_string()];
        let state = store
            .create_run("run-a", &agents, "sha-1", "worktree")
            .expect("create");
        assert!(!state.is_terminal());
        assert!(store.load_manifest("run-a").expect("load").is_some());
        assert_eq!(
            store.read_findings("run-a").expect("findings"),
            Some(String::new())
        );

        // Double-create must fail loudly.
        let err = store
            .create_run("run-a", &agents, "sha-1", "worktree")
            .expect_err("duplicate create");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        let rows = vec![
            ReviewerStateEntry {
                slug: "codex".into(),
                outcome: Some(ReviewerOutcome::Ok),
                exit_code: Some(0),
                stdout_truncated: false,
                stderr_truncated: false,
                launch_error: None,
                pid: None,
                pgid: None,
                proc_start_ticks: None,
            },
            ReviewerStateEntry {
                slug: "opencode".into(),
                outcome: Some(ReviewerOutcome::Failed),
                exit_code: Some(2),
                stdout_truncated: true,
                stderr_truncated: false,
                launch_error: None,
                pid: None,
                pgid: None,
                proc_start_ticks: None,
            },
        ];
        store
            .finalize_run(
                "run-a",
                ReviewTerminalState::Partial,
                &rows,
                RedactionReportSummary {
                    matches: 1,
                    bytes_scanned: 10,
                    bytes_redacted: 4,
                },
            )
            .expect("finalize");
        let state = store.load_state("run-a").expect("load").expect("state");
        assert_eq!(state.terminal_state, Some(ReviewTerminalState::Partial));
        assert!(state.updated_at >= state.created_at);
        let manifest = store
            .load_manifest("run-a")
            .expect("load")
            .expect("manifest");
        assert_eq!(manifest.terminal_state, Some(ReviewTerminalState::Partial));
        assert_eq!(manifest.redaction_report.matches, 1);
    }

    #[test]
    fn list_is_keyset_ordered_and_paginates_without_dup_or_loss() {
        let (_dir, store) = store();
        // Force controlled created_at values (same timestamp for b/c to
        // exercise the run_id DESC tiebreak).
        for (run_id, created_at) in [
            ("run-a", "2026-07-01T00:00:00.000000Z"),
            ("run-b", "2026-07-02T00:00:00.000000Z"),
            ("run-c", "2026-07-02T00:00:00.000000Z"),
            ("run-d", "2026-07-03T00:00:00.000000Z"),
        ] {
            let mut state = store
                .create_run(run_id, &["codex".to_string()], "sha", "scope")
                .expect("create");
            state.created_at = created_at.to_string();
            state.updated_at = created_at.to_string();
            store.write_state(&state).expect("write");
        }

        let all = store.list_runs().expect("list");
        let ids: Vec<&str> = all.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(
            ids,
            ["run-d", "run-c", "run-b", "run-a"],
            "created_at DESC then run_id DESC"
        );

        // Walk pages of 3 then the remainder: no duplicates, no loss.
        let page1 = store.list_runs_page(None, 3).expect("page1");
        assert_eq!(page1.items.len(), 3);
        assert!(page1.has_more);
        let cursor = page1.next_cursor.clone().expect("cursor");
        assert_eq!(cursor.run_id, "run-b");
        let page2 = store.list_runs_page(Some(&cursor), 3).expect("page2");
        assert_eq!(page2.items.len(), 1);
        assert!(!page2.has_more);
        assert!(page2.next_cursor.is_none());
        let mut walked: Vec<String> = page1
            .items
            .iter()
            .chain(page2.items.iter())
            .map(|r| r.run_id.clone())
            .collect();
        walked.sort();
        let mut expected: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(walked, expected);
    }

    #[test]
    fn cancel_mark_and_clean() {
        let (_dir, store) = store();
        store
            .create_run("run-x", &["codex".to_string()], "sha", "scope")
            .expect("create");

        assert!(!store.cancel_requested("run-x"));
        assert!(store.mark_cancel_requested("run-x").expect("mark"));
        assert!(store.cancel_requested("run-x"));
        assert!(
            !store
                .mark_cancel_requested("missing")
                .expect("mark missing")
        );

        assert!(store.mark_cancelled("run-x").expect("cancel"));
        let state = store.load_state("run-x").expect("load").expect("state");
        assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
        assert_eq!(
            state.agents[0].outcome,
            Some(ReviewerOutcome::Cancelled),
            "pending reviewers are marked cancelled"
        );
        // Idempotent: already terminal.
        assert!(!store.mark_cancelled("run-x").expect("cancel again"));

        assert!(store.clean_run("run-x").expect("clean"));
        assert!(!store.clean_run("run-x").expect("clean missing"));
        store
            .create_run("run-y", &["codex".to_string()], "sha", "scope")
            .expect("create");
        assert_eq!(store.clean_all().expect("clean all"), 1);
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
        assert_eq!(sanitize_reviewer_name("Claude Code!"), "claude-code-");
        assert_eq!(sanitize_reviewer_name("../x"), "---x");
        assert_eq!(sanitize_reviewer_name(""), "reviewer");
    }
}
