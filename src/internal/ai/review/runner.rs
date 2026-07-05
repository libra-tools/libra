//! The review run loop: concurrent reviewers, fan-in to a serial sink,
//! five terminal states, one shared cancel/cleanup path, and the
//! `agent.review.run` span.
//!
//! # Flow
//!
//! 1. Validate the request (at least one reviewer; every built-in slug
//!    must be `launchable_review` in the capability matrix — anything
//!    else is a structured error before any side effect, plan.md:945).
//! 2. Create the run directory (`store::ReviewRunStore::create_run`).
//! 3. Materialize the **mandatory** isolated workspace through the
//!    [`materialize_isolated_workspace`] seam (plan.md:946) and record
//!    its root in `state.json` so an orphaned-run cancel can release it.
//!    The FUSE backend is force-disabled: AG-22 pins the copy backend,
//!    whose ignore-aware walk provably excludes gitignored files (the
//!    `.env.test` exclusion proof lives in `orchestrator/workspace.rs`
//!    tests); a FUSE overlay would need its own ignored-file-exposure
//!    proof before it may carry reviewers.
//! 4. Spawn reviewers concurrently (bounded by
//!    [`DEFAULT_MAX_CONCURRENT_REVIEWERS`]-defaulted semaphore,
//!    `agent.md:519-525` `max_reviewers_per_run`), write each spawned
//!    pid/pgid through to `state.json` (serially, via the sink); each
//!    reviewer's stdout/stderr drains into its own 64 KiB bounded buffer
//!    and fans in to a single serial sink that writes the redacted logs.
//! 5. Aggregate per-reviewer outcomes into exactly one of
//!    `success` / `error` / `cancelled` / `timeout` / `partial`
//!    ([`store::aggregate_terminal_state`]), compose `findings.md`
//!    (raw-redacted, spotlighting delimiters, provenance=untrusted),
//!    finalize `state.json` + `manifest.json`, release the workspace.
//!
//! # Cancel
//!
//! [`ReviewCancelHandle`] is the single cleanup entry for a live run:
//! `review cancel` and foreground SIGINT/SIGTERM both call
//! [`ReviewCancelHandle::cancel`]; a cross-process
//! `review cancel <run_id>` writes the store's cancel marker, which the
//! runner polls. Either way the same path kills reviewer process groups,
//! drains and joins the reader tasks, releases the workspace, and marks
//! the run `cancelled`. When no runner is alive to answer the marker,
//! [`cancel_orphaned_run`] releases what the dead runner recorded
//! (process groups + workspace) instead of merely stamping the state.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::{Notify, Semaphore, mpsc};
use tracing::Instrument;

use super::{
    launcher::{
        CODEX_LAST_MESSAGE_FILE, DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD, DEFAULT_REVIEWER_TIMEOUT,
        ReviewerCommand, ReviewerLaunchError, ReviewerLaunchPlan, build_reviewer_command,
        is_launchable_reviewer, kill_process_group, process_group_alive, process_start_ticks,
        spawn_reviewer, unsupported_reviewer_error,
    },
    sink::{
        BoundedSinkBuffer, REVIEW_SINK_BUFFER_BYTES, REVIEW_SINK_TRUNCATION_MARKER, drain_capped,
        findings_section, redact_untrusted, scrub_controls,
    },
    store::{
        RedactionReportSummary, ReviewRunStore, ReviewTerminalState, ReviewerOutcome,
        ReviewerStateEntry, aggregate_terminal_state, sanitize_reviewer_name,
    },
};
use crate::internal::ai::{
    agent::runtime::{
        WorkspaceIsolationConfig, sub_agent_dispatcher::materialize_isolated_workspace,
    },
    agent_run::AgentRunId,
    orchestrator::workspace::{FuseProvisionState, SubAgentWorkspace},
};

/// Default concurrent-reviewer cap per run (`agent.md:519-525`
/// `max_reviewers_per_run`, default 4). Extra reviewers queue; they are
/// never rejected and never affect already-running reviewers.
pub const DEFAULT_MAX_CONCURRENT_REVIEWERS: usize = 4;

/// How often the runner polls the store's cross-process cancel marker.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// After the reviewer process itself is terminal, how long the reader
/// tasks may keep draining before the process group is killed: a
/// reviewer that spawned a descendant which inherited its stdout/stderr
/// pipes can otherwise hold the drains open forever (the pipes only EOF
/// when the *last* writer closes them).
const POST_EXIT_DRAIN_GRACE: Duration = Duration::from_secs(3);

/// After the post-exit group kill, how long the drains get to observe
/// EOF before the reads are abandoned outright (last-resort bound; on
/// unix the group kill closes the write ends, so this window is
/// normally never exhausted).
const POST_KILL_DRAIN_WINDOW: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Cancel handle
// ---------------------------------------------------------------------------

/// Cloneable cancel signal shared by every cancellation source.
///
/// `review cancel` (same process), foreground SIGINT/SIGTERM wiring, and
/// the cross-process marker poller all funnel into [`Self::cancel`], so
/// there is exactly one cleanup path: kill reviewer process groups,
/// join the reader tasks, release the workspace, mark the run
/// `cancelled`.
#[derive(Clone, Debug, Default)]
pub struct ReviewCancelHandle {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ReviewCancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Resolve once cancellation has been requested.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.notify.notified();
            // Re-check to close the notify race window.
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Request / outcome
// ---------------------------------------------------------------------------

/// How one reviewer's process is produced.
#[derive(Debug, Clone)]
pub enum ReviewerSource {
    /// Production path: the §0.3.2 real-CLI argv is built against the
    /// materialized isolated workspace. Only `launchable_review` slugs
    /// (capability matrix) are accepted; anything else fails validation
    /// before any side effect.
    Builtin { slug: String },
    /// Test seam: a directly constructed command. It still runs inside
    /// the isolated workspace — the spawn pins the working directory
    /// there unconditionally; occurrences of the literal `{workspace}`
    /// in its args are substituted with the workspace root.
    Custom(ReviewerCommand),
}

impl ReviewerSource {
    pub fn slug(&self) -> &str {
        match self {
            Self::Builtin { slug } => slug,
            Self::Custom(command) => &command.slug,
        }
    }
}

/// Inputs for one review run.
#[derive(Debug, Clone)]
pub struct ReviewRunRequest {
    /// The repo worktree reviewers review (mirrored into the isolated
    /// workspace — reviewers never touch this directory itself).
    pub repo_root: PathBuf,
    /// Pre-allocated run id (tests); `None` generates a fresh
    /// [`AgentRunId`].
    pub run_id: Option<AgentRunId>,
    /// Review prompt handed to every built-in reviewer.
    pub prompt: String,
    /// Human-readable scope recorded in state/manifest.
    pub target_scope: String,
    /// Commit the reviewed worktree starts from.
    pub starting_sha: String,
    pub reviewers: Vec<ReviewerSource>,
    /// Per-reviewer wall-clock budget applied to built-in reviewers.
    pub reviewer_timeout: Duration,
    pub max_concurrent_reviewers: usize,
    /// Whether workspace materialization may fall back to a full copy
    /// when the size-selected strategy is unavailable. Defaults to
    /// `true`: a read-only review must still run on large repos.
    pub allow_full_copy: bool,
    pub claude_max_budget_usd: String,
}

impl ReviewRunRequest {
    pub fn new(
        repo_root: impl Into<PathBuf>,
        prompt: impl Into<String>,
        target_scope: impl Into<String>,
        starting_sha: impl Into<String>,
        reviewers: Vec<ReviewerSource>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            run_id: None,
            prompt: prompt.into(),
            target_scope: target_scope.into(),
            starting_sha: starting_sha.into(),
            reviewers,
            reviewer_timeout: DEFAULT_REVIEWER_TIMEOUT,
            max_concurrent_reviewers: DEFAULT_MAX_CONCURRENT_REVIEWERS,
            allow_full_copy: true,
            claude_max_budget_usd: DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD.to_string(),
        }
    }
}

/// Per-reviewer result.
#[derive(Debug, Clone)]
pub struct ReviewerReport {
    pub slug: String,
    pub outcome: ReviewerOutcome,
    pub exit_code: Option<i32>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// Redacted single-line launch failure, when the reviewer never ran.
    pub launch_error: Option<String>,
}

/// Terminal result of a review run. Note that infrastructure failures
/// *after* the run directory exists still land here (with
/// `terminal_state == Error` and `infra_error` set): every created run
/// ends in exactly one of the five states.
#[derive(Debug)]
pub struct ReviewRunOutcome {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub terminal_state: ReviewTerminalState,
    pub reviewers: Vec<ReviewerReport>,
    pub duration_ms: u64,
    /// Redacted description of an infrastructure failure (workspace
    /// materialization, …) when `terminal_state == Error` was not
    /// produced by reviewer outcomes.
    pub infra_error: Option<String>,
}

/// Failure before a run exists (validation) or while persisting run
/// state (the store itself is broken).
#[derive(Debug, thiserror::Error)]
pub enum ReviewRunError {
    #[error("no reviewers requested; pass at least one agent slug")]
    NoReviewers,
    /// A non-launchable slug was requested. Raised before any side
    /// effect — no run directory, no workspace, no spawn.
    #[error(transparent)]
    UnsupportedReviewer(ReviewerLaunchError),
    #[error("failed to persist review run state: {0}")]
    Store(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Execute one review run to a terminal state.
///
/// Emits one `agent.review.run` span (`agent.md` §6 :1334) carrying
/// `run_id`, `agent_count`, `terminal_state`, `duration_ms` — reviewer
/// raw stdout is a FORBIDDEN field and is never recorded on the span.
pub async fn run_review(
    store: &ReviewRunStore,
    request: ReviewRunRequest,
    cancel: ReviewCancelHandle,
) -> Result<ReviewRunOutcome, ReviewRunError> {
    let span = tracing::info_span!(
        "agent.review.run",
        run_id = tracing::field::Empty,
        agent_count = tracing::field::Empty,
        terminal_state = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    );
    run_review_inner(store, request, cancel)
        .instrument(span)
        .await
}

async fn run_review_inner(
    store: &ReviewRunStore,
    request: ReviewRunRequest,
    cancel: ReviewCancelHandle,
) -> Result<ReviewRunOutcome, ReviewRunError> {
    let started = Instant::now();

    // ---- Validation: structured failures before any side effect. ----
    if request.reviewers.is_empty() {
        return Err(ReviewRunError::NoReviewers);
    }
    for source in &request.reviewers {
        if let ReviewerSource::Builtin { slug } = source
            && !is_launchable_reviewer(slug)
        {
            return Err(ReviewRunError::UnsupportedReviewer(
                unsupported_reviewer_error(slug),
            ));
        }
    }

    // ---- Run directory. ----
    let run_id = request.run_id.unwrap_or_default();
    let run_id_str = run_id.0.to_string();
    let span = tracing::Span::current();
    span.record("run_id", run_id_str.as_str());
    span.record("agent_count", request.reviewers.len() as u64);

    let slugs: Vec<String> = request
        .reviewers
        .iter()
        .map(|source| source.slug().to_string())
        .collect();
    store.create_run(
        &run_id_str,
        &slugs,
        &request.starting_sha,
        &request.target_scope,
    )?;
    let run_dir = store.run_dir(&run_id_str)?;
    let reviewers_dir = store.reviewers_dir(&run_id_str)?;

    // Unique, filesystem-safe log names per reviewer (duplicate slugs
    // get an index suffix) — pre-created so the E8 file set exists even
    // for reviewers that never produce output.
    let mut log_paths: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(slugs.len());
    {
        let mut used: Vec<String> = Vec::new();
        for (index, slug) in slugs.iter().enumerate() {
            let mut name = sanitize_reviewer_name(slug);
            if used.contains(&name) {
                name = format!("{name}-{index}");
            }
            used.push(name.clone());
            let stdout_log = store.reviewer_stdout_log_path(&run_id_str, &name)?;
            let stderr_log = store.reviewer_stderr_log_path(&run_id_str, &name)?;
            std::fs::write(&stdout_log, b"")?;
            std::fs::write(&stderr_log, b"")?;
            log_paths.push((stdout_log, stderr_log));
        }
    }

    // Helper: finalize + record span fields, exactly once per exit path.
    let finish = |terminal: ReviewTerminalState| {
        let span = tracing::Span::current();
        span.record("terminal_state", terminal.as_str());
        span.record("duration_ms", started.elapsed().as_millis() as u64);
    };

    // ---- Early cancel (marker may pre-date the run; the handle may
    // already be tripped by a signal). ----
    if cancel.is_cancelled() || store.cancel_requested(&run_id_str) {
        let reports = cancelled_reports(&slugs);
        let outcome = finalize(
            store,
            &run_id_str,
            &run_dir,
            true,
            reports,
            HashMap::new(),
            RedactionReportSummary::default(),
            &request,
            started,
            None,
        )?;
        finish(outcome.terminal_state);
        return Ok(outcome);
    }

    // ---- Mandatory isolated workspace (plan.md:946/:947). ----
    // The copy backend is pinned: it materializes through an
    // ignore-aware walk, so gitignored secret files never enter the
    // workspace. (FUSE would need its own exposure proof first.)
    let fuse_state = FuseProvisionState::default();
    let _ = fuse_state.disable_first_time();
    let isolation = WorkspaceIsolationConfig {
        fuse_state,
        sessions_root: store.sessions_root().to_path_buf(),
        allow_full_copy: request.allow_full_copy,
    };
    let repo_root = request.repo_root.clone();
    let thread_id = run_id.0;
    let materialized = tokio::task::spawn_blocking(move || {
        materialize_isolated_workspace(&repo_root, thread_id, run_id, &isolation)
    })
    .await;
    let workspace = match materialized {
        Ok(Ok(workspace)) => workspace,
        Ok(Err(err)) => {
            let outcome = finalize(
                store,
                &run_id_str,
                &run_dir,
                false,
                Vec::new(),
                HashMap::new(),
                RedactionReportSummary::default(),
                &request,
                started,
                Some(format!("workspace materialization failed: {err}")),
            )?;
            finish(outcome.terminal_state);
            return Ok(outcome);
        }
        Err(join_err) => {
            let outcome = finalize(
                store,
                &run_id_str,
                &run_dir,
                false,
                Vec::new(),
                HashMap::new(),
                RedactionReportSummary::default(),
                &request,
                started,
                Some(format!(
                    "workspace materialization task panicked: {join_err}"
                )),
            )?;
            finish(outcome.terminal_state);
            return Ok(outcome);
        }
    };
    let mut workspace_guard = ReviewWorkspaceGuard {
        workspace: Some(workspace),
    };
    let workspace_root = workspace_guard
        .workspace
        .as_ref()
        .map(|ws| ws.root().to_path_buf())
        .unwrap_or_default();
    // Record the workspace root so an orphaned-run cancel (this process
    // dies without finalizing) can still release the directory.
    store.update_state(&run_id_str, |state| {
        state.workspace_root = Some(workspace_root.display().to_string());
    })?;

    // ---- Build the final commands against the workspace. ----
    let plan = ReviewerLaunchPlan {
        workspace_root: workspace_root.clone(),
        prompt: request.prompt.clone(),
        scratch_dir: reviewers_dir.clone(),
        run_title: format!("libra-review-{run_id_str}"),
        claude_max_budget_usd: request.claude_max_budget_usd.clone(),
        timeout: request.reviewer_timeout,
    };
    let mut commands: Vec<ReviewerCommand> = Vec::with_capacity(request.reviewers.len());
    for source in &request.reviewers {
        match source {
            ReviewerSource::Builtin { slug } => {
                // Slugs were validated above; a failure here is a logic
                // error surfaced as a per-reviewer launch failure below
                // rather than a panic.
                match build_reviewer_command(slug, &plan) {
                    Ok(command) => commands.push(command),
                    Err(err) => {
                        commands.push(ReviewerCommand {
                            slug: slug.clone(),
                            program: PathBuf::from(format!("libra-unsupported-{slug}")),
                            args: vec![format!("{err}")],
                            env: Vec::new(),
                            timeout: request.reviewer_timeout,
                        });
                    }
                }
            }
            ReviewerSource::Custom(command) => {
                let ws = workspace_root.display().to_string();
                let mut command = command.clone();
                command.args = command
                    .args
                    .iter()
                    .map(|arg| arg.replace("{workspace}", &ws))
                    .collect();
                commands.push(command);
            }
        }
    }

    // ---- Cross-process cancel marker poller. ----
    let poller = {
        let store = store.clone();
        let run_id = run_id_str.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                if store.cancel_requested(&run_id) {
                    cancel.cancel();
                    return;
                }
                tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
            }
        })
    };

    // ---- Fan-out reviewers; fan-in to the serial sink. ----
    let (sink_tx, sink_rx) = mpsc::unbounded_channel::<SinkEvent>();
    let sink_task = tokio::spawn(run_sink(
        sink_rx,
        store.clone(),
        run_id_str.clone(),
        log_paths,
    ));

    let semaphore = Arc::new(Semaphore::new(request.max_concurrent_reviewers.max(1)));
    let mut handles = Vec::with_capacity(commands.len());
    for (index, command) in commands.into_iter().enumerate() {
        let semaphore = Arc::clone(&semaphore);
        let cancel = cancel.clone();
        let sink_tx = sink_tx.clone();
        let workspace_root = workspace_root.clone();
        handles.push(tokio::spawn(async move {
            run_one_reviewer(index, command, workspace_root, semaphore, cancel, sink_tx).await
        }));
    }
    drop(sink_tx);

    let mut reports: Vec<Option<ReviewerReport>> = (0..handles.len()).map(|_| None).collect();
    for (index, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok((i, report)) => reports[i] = Some(report),
            Err(join_err) => {
                reports[index] = Some(ReviewerReport {
                    slug: slugs.get(index).cloned().unwrap_or_default(),
                    outcome: ReviewerOutcome::Failed,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    launch_error: Some(format!("reviewer task panicked: {join_err}")),
                });
            }
        }
    }
    let mut reports: Vec<ReviewerReport> = reports
        .into_iter()
        .enumerate()
        .map(|(index, report)| {
            report.unwrap_or(ReviewerReport {
                slug: slugs.get(index).cloned().unwrap_or_default(),
                outcome: ReviewerOutcome::Failed,
                exit_code: None,
                stdout_truncated: false,
                stderr_truncated: false,
                launch_error: Some("reviewer produced no report".to_string()),
            })
        })
        .collect();

    // All senders are gone; the sink drains its queue and returns.
    let sink_output = match sink_task.await {
        Ok(output) => output,
        Err(join_err) => {
            tracing::warn!(error = %join_err, "review sink task panicked; logs may be incomplete");
            SinkOutput::default()
        }
    };
    poller.abort();

    // Merge sink truncation knowledge into the reports.
    for (index, report) in reports.iter_mut().enumerate() {
        if let Some(truncated) = sink_output.stdout_truncated.get(&index) {
            report.stdout_truncated = *truncated;
        }
        if let Some(truncated) = sink_output.stderr_truncated.get(&index) {
            report.stderr_truncated = *truncated;
        }
    }

    // ---- Release the workspace (shared by every exit, including the
    // cancel path — the guard's Drop is the panic backstop). ----
    workspace_guard.release().await;

    // Raw reviewer-CLI side outputs (codex `-o`) bypass redaction and
    // must never survive the run.
    let _ = std::fs::remove_file(reviewers_dir.join(CODEX_LAST_MESSAGE_FILE));

    let outcome = finalize(
        store,
        &run_id_str,
        &run_dir,
        cancel.is_cancelled(),
        reports,
        sink_output.stdout_excerpts,
        sink_output.redaction,
        &request,
        started,
        None,
    )?;
    finish(outcome.terminal_state);
    Ok(outcome)
}

fn cancelled_reports(slugs: &[String]) -> Vec<ReviewerReport> {
    slugs
        .iter()
        .map(|slug| ReviewerReport {
            slug: slug.clone(),
            outcome: ReviewerOutcome::Cancelled,
            exit_code: None,
            stdout_truncated: false,
            stderr_truncated: false,
            launch_error: None,
        })
        .collect()
}

/// Write `findings.md`, stamp `state.json` + `manifest.json` terminal,
/// and assemble the outcome. Used by every exit path so the terminal
/// bookkeeping cannot diverge between success/cancel/error flows.
#[allow(clippy::too_many_arguments)]
fn finalize(
    store: &ReviewRunStore,
    run_id: &str,
    run_dir: &std::path::Path,
    cancelled: bool,
    reports: Vec<ReviewerReport>,
    stdout_excerpts: HashMap<usize, String>,
    mut redaction: RedactionReportSummary,
    request: &ReviewRunRequest,
    started: Instant,
    infra_error: Option<String>,
) -> Result<ReviewRunOutcome, ReviewRunError> {
    let outcomes: Vec<ReviewerOutcome> = reports.iter().map(|report| report.outcome).collect();
    let terminal = if infra_error.is_some() {
        ReviewTerminalState::Error
    } else {
        aggregate_terminal_state(cancelled, &outcomes)
    };

    // findings.md: raw-redacted reviewer stdout in request order, inside
    // spotlighting delimiters. provenance=untrusted — display must go
    // through `sink::render_untrusted_findings`.
    let mut findings = format!(
        "# Review findings\n\n- run_id: {run_id}\n- target_scope: {scope}\n- starting_sha: \
         {sha}\n- terminal_state: {terminal}\n\n",
        scope = request.target_scope,
        sha = request.starting_sha,
    );
    if let Some(infra) = &infra_error {
        findings.push_str(&format!(
            "infrastructure error: {}\n\n",
            scrub_controls(infra)
        ));
    }
    for (index, report) in reports.iter().enumerate() {
        let status_line = match report.outcome {
            ReviewerOutcome::Ok => match report.exit_code {
                Some(code) => format!("ok (exit code {code})"),
                None => "ok".to_string(),
            },
            ReviewerOutcome::Failed => match (&report.launch_error, report.exit_code) {
                (Some(launch), _) => format!("failed to launch: {}", scrub_controls(launch)),
                (None, Some(code)) => format!("failed (exit code {code})"),
                (None, None) => "failed".to_string(),
            },
            ReviewerOutcome::TimedOut => "timed out".to_string(),
            ReviewerOutcome::Cancelled => "cancelled".to_string(),
        };
        let excerpt = stdout_excerpts
            .get(&index)
            .map(String::as_str)
            .unwrap_or("");
        findings.push_str(&findings_section(
            &report.slug,
            &status_line,
            excerpt,
            report.stdout_truncated,
        ));
        findings.push('\n');
    }
    let (_, findings_redaction) = redact_untrusted(findings.as_bytes());
    // The excerpts were already redacted; this pass only accounts for
    // anything the composition itself could have introduced (it should
    // find nothing) — keep the scan bytes out of the aggregate to avoid
    // double counting, but surface any unexpected late match.
    if findings_redaction.matches > 0 {
        redaction.merge(&findings_redaction);
    }
    store.write_findings(run_id, &findings)?;

    let entries: Vec<ReviewerStateEntry> = reports
        .iter()
        .map(|report| ReviewerStateEntry {
            slug: report.slug.clone(),
            outcome: Some(report.outcome),
            exit_code: report.exit_code,
            stdout_truncated: report.stdout_truncated,
            stderr_truncated: report.stderr_truncated,
            launch_error: report.launch_error.clone(),
            // Terminal rows carry no live process handles.
            pid: None,
            pgid: None,
            proc_start_ticks: None,
        })
        .collect();
    store.finalize_run(run_id, terminal, &entries, redaction)?;

    Ok(ReviewRunOutcome {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        terminal_state: terminal,
        reviewers: reports,
        duration_ms: started.elapsed().as_millis() as u64,
        infra_error,
    })
}

// ---------------------------------------------------------------------------
// Orphaned-run cancel (no live runner)
// ---------------------------------------------------------------------------

/// What [`cancel_orphaned_run`] did about the recorded workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrphanedWorkspaceAction {
    /// The run never recorded a workspace (the runner died before
    /// materialization).
    NoneRecorded,
    /// The recorded workspace directory existed, resolved (after
    /// symlink canonicalization) to a `libra-task-worktree-*` directory
    /// INSIDE this repo's task-worktree base, and was removed.
    Removed,
    /// The recorded workspace directory no longer exists (the dead
    /// runner — or a previous cancel — already released it).
    AlreadyGone,
    /// The recorded path was NOT touched: it does not carry the Libra
    /// task-worktree shape, or it does not canonicalize to a location
    /// inside this repo's task-worktree base. A corrupted `state.json`
    /// must never delete arbitrary directories — not even a same-named
    /// `libra-task-worktree-*` directory somewhere else.
    RefusedSuspiciousPath,
}

/// Honest report of an orphaned-run cancel: exactly what was released
/// and what could not be verified. Serializes for `--json` output.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanedRunCancel {
    /// Whether the run transitioned to `cancelled` (false: it was
    /// already terminal — nothing else was touched).
    pub transitioned: bool,
    /// Recorded reviewer process groups whose leader still carried the
    /// recorded start-time provenance and were therefore SIGKILLed
    /// (whole group, so reviewer descendants die too).
    pub killed_pgids: Vec<u32>,
    /// Recorded process groups that were no longer alive — nothing to
    /// kill.
    pub stale_pgids: Vec<u32>,
    /// Recorded process groups that are alive but could NOT be verified
    /// as ours (start-time provenance mismatch: the pid was reused by
    /// an unrelated process — or provenance was unrecorded/unreadable,
    /// e.g. pre-provenance runs or non-Linux). These are NEVER killed;
    /// the caller must surface them for manual inspection.
    pub stale_unsafe_pgids: Vec<u32>,
    /// `false` means the dead runner never recorded any spawned
    /// reviewer (it crashed before spawning) — there was nothing to
    /// kill and nothing to verify.
    pub had_recorded_processes: bool,
    pub workspace_action: OrphanedWorkspaceAction,
    /// The path the workspace action refers to (the removable cleanup
    /// root, or the recorded path verbatim when refused).
    pub workspace_path: Option<String>,
}

/// Cancel a run whose owning runner is gone (crashed or SIGKILLed
/// before finalizing): kill the recorded reviewer process groups that
/// are still alive AND provably ours (pid start-time provenance),
/// remove the recorded isolated-workspace directory (only when it
/// canonicalizes into this repo's task-worktree base), and stamp the
/// run `cancelled` — then report exactly what happened.
///
/// This is the honest counterpart of the live-run cancel (the marker +
/// [`ReviewCancelHandle`] path): the CLI first gives a live runner the
/// chance to acknowledge the marker; only when nothing answers does it
/// call this. Already-terminal runs are left untouched
/// (`transitioned == false`).
///
/// # Safety posture (fail-closed on both resources)
///
/// - **Processes**: pids are reused. A recorded pgid is killed only if
///   the process at that pid *currently* reports the same
///   `/proc/<pid>/stat` start time the runner recorded at spawn
///   ([`process_start_ticks`]). Mismatch, missing recorded provenance
///   (old runs), or an unverifiable platform (no `/proc`) →
///   `stale_unsafe_pgids`, never a kill.
/// - **Workspace**: the recorded path must both carry the
///   `libra-task-worktree-*` shape AND canonicalize (symlinks resolved)
///   to a path inside this repo's own task-worktree base
///   (`<storage>/worktrees/tasks`, derived from the store — the same
///   base the engine materializes under). Anything else is refused.
pub fn cancel_orphaned_run(store: &ReviewRunStore, run_id: &str) -> io::Result<OrphanedRunCancel> {
    let state = store.load_state(run_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("review run '{run_id}' not found (run `libra review list`)"),
        )
    })?;
    if state.is_terminal() {
        return Ok(OrphanedRunCancel {
            transitioned: false,
            killed_pgids: Vec::new(),
            stale_pgids: Vec::new(),
            stale_unsafe_pgids: Vec::new(),
            had_recorded_processes: false,
            workspace_action: OrphanedWorkspaceAction::NoneRecorded,
            workspace_path: None,
        });
    }

    // (a) Release recorded reviewer process groups — with provenance.
    // pids/pgids + start ticks were written through at spawn time by
    // the (now dead) runner's sink. Dedupe by pgid, keeping the first
    // recorded provenance (one entry per spawned process).
    let mut recorded: Vec<(u32, Option<u64>)> = Vec::new();
    for entry in &state.agents {
        if let Some(pgid) = entry.pgid.or(entry.pid)
            && !recorded.iter().any(|(existing, _)| *existing == pgid)
        {
            recorded.push((pgid, entry.proc_start_ticks));
        }
    }
    let had_recorded_processes = !recorded.is_empty();
    let mut killed_pgids = Vec::new();
    let mut stale_pgids = Vec::new();
    let mut stale_unsafe_pgids = Vec::new();
    for (pgid, recorded_ticks) in recorded {
        if !process_group_alive(pgid) {
            stale_pgids.push(pgid);
            continue;
        }
        // The group leader's CURRENT start time must equal the recorded
        // one — otherwise the pid was reused by an unrelated process
        // (or we simply cannot prove it wasn't): never kill on doubt.
        // Note: if the leader already died while descendants keep the
        // group alive, /proc/<pgid> is gone → unverifiable → refused
        // (conservative; reported for manual action).
        let verified = matches!(
            (recorded_ticks, process_start_ticks(pgid)),
            (Some(recorded), Some(current)) if recorded == current
        );
        if verified {
            kill_process_group(pgid);
            killed_pgids.push(pgid);
        } else {
            stale_unsafe_pgids.push(pgid);
        }
    }

    // (b) Release the recorded workspace, confined to this repo's own
    // task-worktree base.
    let allowed_base = task_worktree_base_for_store(store);
    let (workspace_action, workspace_path) = match state.workspace_root.as_deref() {
        None => (OrphanedWorkspaceAction::NoneRecorded, None),
        Some(recorded) => {
            match resolve_orphaned_workspace(Path::new(recorded), allowed_base.as_deref()) {
                OrphanedWorkspaceResolution::Remove(cleanup_root) => {
                    let display = cleanup_root.display().to_string();
                    std::fs::remove_dir_all(&cleanup_root)?;
                    (OrphanedWorkspaceAction::Removed, Some(display))
                }
                OrphanedWorkspaceResolution::AlreadyGone(cleanup_root) => (
                    OrphanedWorkspaceAction::AlreadyGone,
                    Some(cleanup_root.display().to_string()),
                ),
                OrphanedWorkspaceResolution::Refused => (
                    OrphanedWorkspaceAction::RefusedSuspiciousPath,
                    Some(recorded.to_string()),
                ),
            }
        }
    };

    // (c) Stamp the terminal state (pending reviewers → cancelled).
    let transitioned = store.mark_cancelled(run_id)?;

    Ok(OrphanedRunCancel {
        transitioned,
        killed_pgids,
        stale_pgids,
        stale_unsafe_pgids,
        had_recorded_processes,
        workspace_action,
        workspace_path,
    })
}

/// The canonical base this repo's isolated workspaces are materialized
/// under: `<storage>/worktrees/tasks`, where `<storage>` is the
/// `.libra` directory the store's `sessions` root lives in (mirrors
/// `orchestrator::workspace::task_worktree_base_dir`). `None` when the
/// store has no parent (degenerate root path) — every removal is then
/// refused.
fn task_worktree_base_for_store(store: &ReviewRunStore) -> Option<PathBuf> {
    store
        .sessions_root()
        .parent()
        .map(|storage| storage.join("worktrees").join("tasks"))
}

/// Outcome of resolving a recorded workspace path against the allowed
/// base.
#[derive(Debug, PartialEq, Eq)]
enum OrphanedWorkspaceResolution {
    /// Shape and confinement verified; remove this directory.
    Remove(PathBuf),
    /// Nothing on disk to remove (and the location is attributable to
    /// our base).
    AlreadyGone(PathBuf),
    /// Do not touch anything.
    Refused,
}

/// Resolve the removable cleanup root for a recorded workspace path,
/// fail-closed on BOTH the name shape and the location:
///
/// 1. **Shape**: the runner records
///    `<base>/libra-task-worktree-<...>/workspace`; the removable unit
///    is the parent `libra-task-worktree-*` directory (it also holds
///    copy/lower/upper bookkeeping). A path that already names the
///    `libra-task-worktree-*` directory is accepted as-is. Any other
///    shape is refused.
/// 2. **Confinement**: the candidate must canonicalize (symlinks
///    resolved) to a strict descendant of the canonicalized
///    `allowed_base` — the task-worktree base the engine itself
///    materializes under. A same-named directory anywhere else (a
///    corrupted or attacker-written `state.json`) is refused, as is a
///    candidate that is a symlink pointing outside the base.
///
/// A candidate that no longer exists is `AlreadyGone` when its parent
/// canonicalizes to the allowed base (attributable to us; nothing to
/// remove), otherwise refused.
fn resolve_orphaned_workspace(
    recorded: &Path,
    allowed_base: Option<&Path>,
) -> OrphanedWorkspaceResolution {
    const PREFIX: &str = "libra-task-worktree-";
    let file_name = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
    };

    // 1. Shape check → candidate cleanup root.
    let Some(name) = file_name(recorded) else {
        return OrphanedWorkspaceResolution::Refused;
    };
    let cleanup_root = if name.starts_with(PREFIX) {
        recorded.to_path_buf()
    } else if name == "workspace" {
        let Some(parent) = recorded.parent() else {
            return OrphanedWorkspaceResolution::Refused;
        };
        match file_name(parent) {
            Some(parent_name) if parent_name.starts_with(PREFIX) => parent.to_path_buf(),
            _ => return OrphanedWorkspaceResolution::Refused,
        }
    } else {
        return OrphanedWorkspaceResolution::Refused;
    };

    // 2. Confinement check against the canonicalized allowed base.
    let Some(allowed_base) = allowed_base else {
        return OrphanedWorkspaceResolution::Refused;
    };
    let Ok(canonical_base) = std::fs::canonicalize(allowed_base) else {
        // The base does not exist: nothing we materialized can live in
        // it. If the candidate is gone too there is nothing to do;
        // anything still on disk cannot be attributed to us — refuse.
        return if cleanup_root.exists() {
            OrphanedWorkspaceResolution::Refused
        } else {
            OrphanedWorkspaceResolution::AlreadyGone(cleanup_root)
        };
    };
    // A symlinked cleanup root is refused OUTRIGHT before any
    // canonicalization: a link inside the base pointing at another
    // workspace inside the base would pass the confinement check yet
    // delete the victim, not the recorded run's directory. Only a real
    // directory that the engine itself materialized is removable.
    if std::fs::symlink_metadata(&cleanup_root)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return OrphanedWorkspaceResolution::Refused;
    }
    if cleanup_root.exists() {
        match std::fs::canonicalize(&cleanup_root) {
            Ok(canonical)
                if canonical.starts_with(&canonical_base) && canonical != canonical_base =>
            {
                OrphanedWorkspaceResolution::Remove(canonical)
            }
            _ => OrphanedWorkspaceResolution::Refused,
        }
    } else {
        // Leaf gone: attribute via the parent (per the leaf-may-not-
        // exist canonicalization rule).
        match cleanup_root
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
        {
            Some(parent) if parent.starts_with(&canonical_base) => {
                OrphanedWorkspaceResolution::AlreadyGone(cleanup_root)
            }
            _ => OrphanedWorkspaceResolution::Refused,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-reviewer job + serial sink
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
}

/// One reviewer stream's capped capture, fanned in to the serial sink.
struct StreamCapture {
    index: usize,
    kind: StreamKind,
    data: Vec<u8>,
    truncated: bool,
}

/// Fan-in message to the single serial sink: spawn write-throughs and
/// stream captures share the channel so `state.json` has exactly one
/// writer while reviewers run.
enum SinkEvent {
    /// A reviewer process spawned; record its pid/pgid + start-time
    /// provenance in `state.json` so an orphaned-run cancel can release
    /// it safely (the spawn puts every reviewer in its own process
    /// group, so pgid == pid; `start_ticks` is what stops a reused pid
    /// from ever being killed).
    Spawned {
        index: usize,
        pid: u32,
        start_ticks: Option<u64>,
    },
    Stream(StreamCapture),
}

#[derive(Default)]
struct SinkOutput {
    /// Raw-redacted stdout excerpt per reviewer index (findings input).
    stdout_excerpts: HashMap<usize, String>,
    stdout_truncated: HashMap<usize, bool>,
    stderr_truncated: HashMap<usize, bool>,
    redaction: RedactionReportSummary,
}

/// The single serial writer: consumes fan-in events one at a time —
/// spawn write-throughs into `state.json`, redacted captures into the
/// run's reviewer logs. I/O failures are warnings, never run failures —
/// and because every producer buffer is already capped, a slow disk can
/// only delay, never block, reviewers.
async fn run_sink(
    mut rx: mpsc::UnboundedReceiver<SinkEvent>,
    store: ReviewRunStore,
    run_id: String,
    log_paths: Vec<(PathBuf, PathBuf)>,
) -> SinkOutput {
    let mut output = SinkOutput::default();
    while let Some(event) = rx.recv().await {
        let capture = match event {
            SinkEvent::Spawned {
                index,
                pid,
                start_ticks,
            } => {
                if let Err(err) = store.update_state(&run_id, |state| {
                    if let Some(entry) = state.agents.get_mut(index) {
                        entry.pid = Some(pid);
                        entry.pgid = Some(pid);
                        entry.proc_start_ticks = start_ticks;
                    }
                }) {
                    tracing::warn!(
                        error = %err,
                        reviewer_index = index,
                        "failed to write reviewer pid/pgid through to state.json",
                    );
                }
                continue;
            }
            SinkEvent::Stream(capture) => capture,
        };
        let Some((stdout_log, stderr_log)) = log_paths.get(capture.index) else {
            continue;
        };
        let (untrusted, summary) = redact_untrusted(&capture.data);
        output.redaction.merge(&summary);
        let mut log_text = scrub_controls(&untrusted);
        if capture.truncated {
            if !log_text.ends_with('\n') && !log_text.is_empty() {
                log_text.push('\n');
            }
            log_text.push_str(REVIEW_SINK_TRUNCATION_MARKER);
            log_text.push('\n');
        }
        let path = match capture.kind {
            StreamKind::Stdout => stdout_log,
            StreamKind::Stderr => stderr_log,
        };
        if let Err(err) = store.append_reviewer_log(path, &log_text) {
            tracing::warn!(
                error = %err,
                reviewer_index = capture.index,
                "failed to append redacted reviewer log",
            );
        }
        match capture.kind {
            StreamKind::Stdout => {
                output.stdout_excerpts.insert(capture.index, untrusted);
                output
                    .stdout_truncated
                    .insert(capture.index, capture.truncated);
            }
            StreamKind::Stderr => {
                output
                    .stderr_truncated
                    .insert(capture.index, capture.truncated);
            }
        }
    }
    output
}

enum WaitKind {
    Exited(std::io::Result<std::process::ExitStatus>),
    Cancelled,
    TimedOut,
}

/// Run one reviewer to completion: spawn inside the isolated workspace,
/// drain both streams into bounded buffers, enforce the per-reviewer
/// deadline, and honour cancellation (process-group kill).
async fn run_one_reviewer(
    index: usize,
    command: ReviewerCommand,
    workspace_root: PathBuf,
    semaphore: Arc<Semaphore>,
    cancel: ReviewCancelHandle,
    sink_tx: mpsc::UnboundedSender<SinkEvent>,
) -> (usize, ReviewerReport) {
    let slug = command.slug.clone();
    let report = |outcome, exit_code, launch_error| ReviewerReport {
        slug: slug.clone(),
        outcome,
        exit_code,
        stdout_truncated: false,
        stderr_truncated: false,
        launch_error,
    };

    // Queue behind the concurrency cap; a cancel while queued must not
    // spawn at all.
    let _permit = tokio::select! {
        permit = semaphore.acquire_owned() => permit,
        _ = cancel.cancelled() => {
            return (index, report(ReviewerOutcome::Cancelled, None, None));
        }
    };
    if cancel.is_cancelled() {
        return (index, report(ReviewerOutcome::Cancelled, None, None));
    }

    let mut spawned = match spawn_reviewer(&command, &workspace_root).await {
        Ok(spawned) => spawned,
        Err(err) => {
            let (clean, _) = super::sink::redact_for_log(err.to_string().as_bytes());
            return (index, report(ReviewerOutcome::Failed, None, Some(clean)));
        }
    };
    let pgid = spawned.pgid;
    // Write the pid/pgid + start-time provenance through to state.json
    // (serially, via the sink) so an orphaned-run cancel can release
    // this process group without ever killing a reused pid.
    if let Some(pid) = pgid {
        let _ = sink_tx.send(SinkEvent::Spawned {
            index,
            pid,
            start_ticks: spawned.start_ticks,
        });
    }

    // Independent reader tasks: they keep draining no matter what the
    // wait below is doing, and each is bounded by the 64 KiB cap.
    let stdout = spawned.child.stdout.take();
    let stderr = spawned.child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(stream) => drain_capped(stream, REVIEW_SINK_BUFFER_BYTES).await,
            None => BoundedSinkBuffer::new(REVIEW_SINK_BUFFER_BYTES),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(stream) => drain_capped(stream, REVIEW_SINK_BUFFER_BYTES).await,
            None => BoundedSinkBuffer::new(REVIEW_SINK_BUFFER_BYTES),
        }
    });

    // Phase 1: the reviewer process itself, under cancel/timeout
    // enforcement.
    let waited = {
        let kind = tokio::select! {
            status = spawned.child.wait() => WaitKind::Exited(status),
            _ = cancel.cancelled() => WaitKind::Cancelled,
            _ = tokio::time::sleep(command.timeout) => WaitKind::TimedOut,
        };
        match kind {
            WaitKind::Exited(status) => WaitKind::Exited(status),
            other => {
                // Cancel/timeout: take down the whole process group so
                // reviewer children die too, then reap.
                spawned.kill_tree().await;
                other
            }
        }
    };

    // Phase 2: the child is terminal, but a descendant that inherited
    // the stdout/stderr pipes can hold them open indefinitely — the
    // drains only see EOF when the LAST writer closes. Enforcement must
    // outlive the child: grace-join the drains, then kill the process
    // group and re-join, then abandon the reads as a last resort. This
    // bounds every run even against a pipe-squatting descendant.
    let (stdout_buf, stderr_buf) = join_drains_bounded(stdout_task, stderr_task, pgid, &slug).await;

    let _ = sink_tx.send(SinkEvent::Stream(StreamCapture {
        index,
        kind: StreamKind::Stdout,
        truncated: stdout_buf.truncated(),
        data: stdout_buf.into_bytes(),
    }));
    let _ = sink_tx.send(SinkEvent::Stream(StreamCapture {
        index,
        kind: StreamKind::Stderr,
        truncated: stderr_buf.truncated(),
        data: stderr_buf.into_bytes(),
    }));

    let report = match waited {
        WaitKind::Exited(Ok(status)) => {
            if status.success() {
                report(ReviewerOutcome::Ok, status.code(), None)
            } else {
                report(ReviewerOutcome::Failed, status.code(), None)
            }
        }
        WaitKind::Exited(Err(err)) => {
            let (clean, _) = super::sink::redact_for_log(err.to_string().as_bytes());
            report(ReviewerOutcome::Failed, None, Some(clean))
        }
        WaitKind::Cancelled => report(ReviewerOutcome::Cancelled, None, None),
        WaitKind::TimedOut => report(ReviewerOutcome::TimedOut, None, None),
    };
    (index, report)
}

/// Join both drain tasks with the reviewer already terminal, keeping
/// the run bounded when a descendant still holds the pipes:
/// [`POST_EXIT_DRAIN_GRACE`] → process-group kill →
/// [`POST_KILL_DRAIN_WINDOW`] → abandon the reads (abort the tasks).
async fn join_drains_bounded(
    stdout_task: tokio::task::JoinHandle<BoundedSinkBuffer>,
    stderr_task: tokio::task::JoinHandle<BoundedSinkBuffer>,
    pgid: Option<u32>,
    slug: &str,
) -> (BoundedSinkBuffer, BoundedSinkBuffer) {
    let stdout_abort = stdout_task.abort_handle();
    let stderr_abort = stderr_task.abort_handle();
    let joined = async move { tokio::join!(stdout_task, stderr_task) };
    tokio::pin!(joined);
    match tokio::time::timeout(POST_EXIT_DRAIN_GRACE, &mut joined).await {
        Ok((stdout_buf, stderr_buf)) => {
            (take_buffer(stdout_buf, slug), take_buffer(stderr_buf, slug))
        }
        Err(_) => {
            tracing::warn!(
                slug,
                "reviewer exited but its output pipes are still open (a spawned descendant \
                 inherited them); killing the reviewer process group",
            );
            if let Some(pgid) = pgid {
                kill_process_group(pgid);
            }
            match tokio::time::timeout(POST_KILL_DRAIN_WINDOW, &mut joined).await {
                Ok((stdout_buf, stderr_buf)) => {
                    (take_buffer(stdout_buf, slug), take_buffer(stderr_buf, slug))
                }
                Err(_) => {
                    // Last resort (e.g. non-unix, where no group kill
                    // exists): abandon the reads so the run stays
                    // bounded. The captured bytes for this reviewer are
                    // lost; the terminal outcome is unaffected.
                    stdout_abort.abort();
                    stderr_abort.abort();
                    tracing::warn!(
                        slug,
                        "reviewer output pipes still open after the group kill; abandoning \
                         the reads (this reviewer's captured output is lost)",
                    );
                    (
                        BoundedSinkBuffer::new(REVIEW_SINK_BUFFER_BYTES),
                        BoundedSinkBuffer::new(REVIEW_SINK_BUFFER_BYTES),
                    )
                }
            }
        }
    }
}

fn take_buffer(
    result: Result<BoundedSinkBuffer, tokio::task::JoinError>,
    slug: &str,
) -> BoundedSinkBuffer {
    match result {
        Ok(buffer) => buffer,
        Err(join_err) => {
            tracing::warn!(slug, error = %join_err, "reviewer drain task failed");
            BoundedSinkBuffer::new(REVIEW_SINK_BUFFER_BYTES)
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace guard
// ---------------------------------------------------------------------------

/// Panic backstop for the run's isolated workspace, mirroring the
/// sub-agent dispatcher's `WorkspaceCleanupGuard`: the normal path
/// releases explicitly (awaited `spawn_blocking`); `Drop` only fires on
/// unwind. A copy-backend teardown is plain filesystem removal (safe
/// inline); a FUSE teardown must not block the runtime thread, so it is
/// routed to the blocking pool when a runtime exists.
struct ReviewWorkspaceGuard {
    workspace: Option<SubAgentWorkspace>,
}

impl ReviewWorkspaceGuard {
    async fn release(&mut self) {
        if let Some(workspace) = self.workspace.take() {
            match tokio::task::spawn_blocking(move || workspace.cleanup()).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "failed to clean up review workspace");
                }
                Err(join_err) => {
                    tracing::warn!(error = %join_err, "review workspace cleanup task panicked");
                }
            }
        }
    }
}

impl Drop for ReviewWorkspaceGuard {
    fn drop(&mut self) {
        use crate::internal::ai::orchestrator::types::TaskWorkspaceBackend;

        let Some(workspace) = self.workspace.take() else {
            return;
        };
        if matches!(workspace.backend(), TaskWorkspaceBackend::Fuse)
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn_blocking(move || {
                if let Err(err) = workspace.cleanup() {
                    tracing::warn!(error = %err, "failed to clean up review workspace on unwind");
                }
            });
            return;
        }
        if let Err(err) = workspace.cleanup() {
            tracing::warn!(error = %err, "failed to clean up review workspace on unwind");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::review::store::REVIEW_RUN_KIND;

    fn test_store() -> (tempfile::TempDir, ReviewRunStore, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::write(repo.join("README.md"), "hello\n").expect("seed file");
        let store = ReviewRunStore::new(dir.path().join(".libra").join("sessions"));
        (dir, store, repo)
    }

    #[cfg(unix)]
    fn sh_reviewer(slug: &str, script: &str, timeout: Duration) -> ReviewerSource {
        ReviewerSource::Custom(ReviewerCommand {
            slug: slug.to_string(),
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            env: Vec::new(),
            timeout,
        })
    }

    #[tokio::test]
    async fn unsupported_slug_is_rejected_before_any_side_effect() {
        let (_dir, store, repo) = test_store();
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![ReviewerSource::Builtin {
                slug: "gemini".into(),
            }],
        );
        let err = run_review(&store, request, ReviewCancelHandle::new())
            .await
            .expect_err("gemini is not launchable");
        match err {
            ReviewRunError::UnsupportedReviewer(ReviewerLaunchError::UnsupportedSlug {
                slug,
                roster,
            }) => {
                assert_eq!(slug, "gemini");
                assert_eq!(roster, "claude-code, codex, opencode");
            }
            other => panic!("expected UnsupportedSlug, got {other:?}"),
        }
        // No run directory was created.
        assert!(store.list_runs().expect("list").is_empty());

        let err = run_review(
            &store,
            ReviewRunRequest::new(&repo, "review", "scope", "sha", Vec::new()),
            ReviewCancelHandle::new(),
        )
        .await
        .expect_err("empty reviewer list");
        assert!(matches!(err, ReviewRunError::NoReviewers));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mixed_reviewers_produce_partial_with_full_run_wire() {
        let (_dir, store, repo) = test_store();
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "HEAD~1..HEAD",
            "sha-123",
            vec![
                // Proves cwd is the isolated workspace, not the repo root:
                // the workspace mirrors README.md but lives elsewhere.
                sh_reviewer(
                    "good-reviewer",
                    "test -f README.md && pwd && echo finding-ok",
                    Duration::from_secs(30),
                ),
                sh_reviewer(
                    "bad-reviewer",
                    "echo boom >&2; exit 3",
                    Duration::from_secs(30),
                ),
            ],
        );
        let outcome = run_review(&store, request, ReviewCancelHandle::new())
            .await
            .expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Partial);
        assert_eq!(outcome.reviewers.len(), 2);
        assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Ok);
        assert_eq!(outcome.reviewers[1].outcome, ReviewerOutcome::Failed);
        assert_eq!(outcome.reviewers[1].exit_code, Some(3));

        // Run wire on disk.
        let state = store
            .load_state(&outcome.run_id)
            .expect("load")
            .expect("state");
        assert_eq!(state.kind, REVIEW_RUN_KIND);
        assert_eq!(state.terminal_state, Some(ReviewTerminalState::Partial));
        // Terminal rows carry no live process handles.
        assert!(state.agents.iter().all(|entry| entry.pid.is_none()));
        let manifest = store
            .load_manifest(&outcome.run_id)
            .expect("load")
            .expect("manifest");
        assert_eq!(manifest.terminal_state, Some(ReviewTerminalState::Partial));
        assert_eq!(manifest.agents, vec!["good-reviewer", "bad-reviewer"]);

        let findings = store
            .read_findings(&outcome.run_id)
            .expect("read")
            .expect("findings");
        assert!(findings.contains("finding-ok"));
        assert!(findings.contains(super::super::sink::UNTRUSTED_FINDINGS_OPEN_PREFIX));
        // The reviewer ran in the workspace, not the repo root.
        assert!(
            !findings.contains(&format!("\n{}\n", repo.display())),
            "reviewer cwd must be the isolated workspace, not the repo root: {findings}"
        );

        let stdout_log = store
            .reviewer_stdout_log_path(&outcome.run_id, "good-reviewer")
            .expect("path");
        let log = std::fs::read_to_string(stdout_log).expect("stdout log");
        assert!(log.contains("finding-ok"));
        let stderr_log = store
            .reviewer_stderr_log_path(&outcome.run_id, "bad-reviewer")
            .expect("path");
        let log = std::fs::read_to_string(stderr_log).expect("stderr log");
        assert!(log.contains("boom"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_kills_reviewers_and_marks_cancelled() {
        let (_dir, store, repo) = test_store();
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![sh_reviewer("sleepy", "sleep 30", Duration::from_secs(60))],
        );
        let cancel = ReviewCancelHandle::new();
        let started = Instant::now();
        let run = tokio::spawn({
            let store = store.clone();
            let cancel = cancel.clone();
            async move { run_review(&store, request, cancel).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
        let outcome = run.await.expect("join").expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "cancel must not wait for the 30s reviewer"
        );
        let state = store
            .load_state(&outcome.run_id)
            .expect("load")
            .expect("state");
        assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_reviewer_timeout_yields_timeout_terminal_state() {
        let (_dir, store, repo) = test_store();
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![sh_reviewer("slow", "sleep 30", Duration::from_millis(300))],
        );
        let started = Instant::now();
        let outcome = run_review(&store, request, ReviewCancelHandle::new())
            .await
            .expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Timeout);
        assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timeout must not wait for the 30s reviewer"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pre_existing_cancel_marker_cancels_without_spawning() {
        let (_dir, store, repo) = test_store();
        let run_id = AgentRunId::new();
        // Create the run dir out of band so the marker can pre-exist,
        // then hand the same run_id to the runner… the create would
        // collide; instead trip the in-process handle before start,
        // which shares the identical early-cancel path with the marker.
        let cancel = ReviewCancelHandle::new();
        cancel.cancel();
        let mut request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![sh_reviewer(
                "never-runs",
                "echo nope",
                Duration::from_secs(30),
            )],
        );
        request.run_id = Some(run_id);
        let outcome = run_review(&store, request, cancel)
            .await
            .expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Cancelled);
        assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Cancelled);
        let findings = store
            .read_findings(&outcome.run_id)
            .expect("read")
            .expect("findings");
        assert!(
            !findings.contains("nope"),
            "reviewer must never have spawned"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flooding_reviewer_is_capped_with_truncation_marker() {
        let (_dir, store, repo) = test_store();
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![
                // ~1 MiB of output: far past the 64 KiB per-sink cap.
                sh_reviewer(
                    "flooder",
                    "i=0; while [ $i -lt 16384 ]; do echo 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef; i=$((i+1)); done",
                    Duration::from_secs(60),
                ),
                sh_reviewer("quiet", "echo small-finding", Duration::from_secs(60)),
            ],
        );
        let outcome = run_review(&store, request, ReviewCancelHandle::new())
            .await
            .expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Success);
        assert!(outcome.reviewers[0].stdout_truncated);
        assert!(!outcome.reviewers[1].stdout_truncated);
        let stdout_log = store
            .reviewer_stdout_log_path(&outcome.run_id, "flooder")
            .expect("path");
        let log = std::fs::read_to_string(stdout_log).expect("log");
        assert!(log.contains(REVIEW_SINK_TRUNCATION_MARKER));
        // The persisted log stays within the cap plus marker slack.
        assert!(log.len() <= REVIEW_SINK_BUFFER_BYTES + 256);
        // The quiet reviewer was not starved by the flooder.
        let quiet_log = store
            .reviewer_stdout_log_path(&outcome.run_id, "quiet")
            .expect("path");
        assert!(
            std::fs::read_to_string(quiet_log)
                .expect("log")
                .contains("small-finding")
        );
    }

    /// Regression (codex review P1): a reviewer that spawns a
    /// descendant which INHERITS its stdout pipe and then exits must
    /// not hang the run — the post-exit drain grace kills the process
    /// group, the run reaches a terminal state within bounds, and the
    /// descendant is dead afterwards.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn descendant_holding_pipes_does_not_hang_the_run() {
        let (dir, store, repo) = test_store();
        let pidfile = dir.path().join("descendant.pid");
        let script = format!(
            "/bin/sleep 300 & echo $! > {}; echo before-exit; exit 0",
            pidfile.display()
        );
        let request = ReviewRunRequest::new(
            &repo,
            "review",
            "scope",
            "sha",
            vec![sh_reviewer(
                "pipe-holder",
                &script,
                Duration::from_secs(120),
            )],
        );
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            run_review(&store, request, ReviewCancelHandle::new()),
        )
        .await
        .expect("run must not hang on the inherited pipe")
        .expect("run completes");
        assert_eq!(outcome.terminal_state, ReviewTerminalState::Success);
        assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Ok);
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "drain grace must bound the run (took {:?})",
            started.elapsed()
        );

        // The pipe-holding descendant must be dead (group kill). NOTE:
        // the sleeper reparents to init/subreaper when sh exits, so a
        // successful kill(pid, 0) probe here means genuinely alive, not
        // an unreaped zombie of ours.
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("pidfile")
            .trim()
            .parse()
            .expect("descendant pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // SAFETY: signal 0 only probes liveness of a PID we spawned.
            if unsafe { libc::kill(pid, 0) } == -1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "descendant sleeper (pid {pid}) still alive after the run"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Output written before the exit still made it through the sink.
        let findings = store
            .read_findings(&outcome.run_id)
            .expect("read")
            .expect("findings");
        assert!(
            findings.contains("before-exit"),
            "pre-exit output must be captured: {findings}"
        );
    }

    /// Spawn a decoy sleeper in its own process group — stands in for a
    /// reviewer whose runner died without cleaning up.
    #[cfg(unix)]
    fn spawn_decoy_sleeper() -> std::process::Child {
        let mut decoy_cmd = std::process::Command::new("/bin/sleep");
        decoy_cmd.arg("300");
        std::os::unix::process::CommandExt::process_group(&mut decoy_cmd, 0);
        decoy_cmd.spawn().expect("spawn decoy sleeper")
    }

    /// Seed a fake recorded workspace INSIDE the store's task-worktree
    /// base (the only location the orphan cancel will agree to remove)
    /// and return its cleanup root + recorded workspace path.
    fn seed_confined_workspace(store: &ReviewRunStore, name: &str) -> (PathBuf, PathBuf) {
        let base = task_worktree_base_for_store(store).expect("store has a storage parent");
        let cleanup_root = base.join(format!("libra-task-worktree-{name}"));
        let workspace = cleanup_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("fake workspace");
        (cleanup_root, workspace)
    }

    /// Orphaned-run cancel (codex review P1 + R2 provenance): with no
    /// live runner, a recorded pgid whose start-time provenance still
    /// matches is killed, the recorded workspace is removed, the run is
    /// stamped cancelled, and the report says exactly what happened. A
    /// second call is a no-op. Linux-only: provenance verification
    /// requires `/proc`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphaned_run_cancel_kills_verified_pgids_and_removes_workspace() {
        let (_dir, store, _repo) = test_store();
        store
            .create_run("orphan-run", &["decoy".to_string()], "sha", "scope")
            .expect("create");

        let mut decoy = spawn_decoy_sleeper();
        let decoy_pid = decoy.id();
        let decoy_ticks = process_start_ticks(decoy_pid);
        assert!(decoy_ticks.is_some(), "decoy /proc stat must parse");

        let (cleanup_root, workspace) = seed_confined_workspace(&store, "orphan-test");
        store
            .update_state("orphan-run", |state| {
                state.agents[0].pid = Some(decoy_pid);
                state.agents[0].pgid = Some(decoy_pid);
                state.agents[0].proc_start_ticks = decoy_ticks;
                state.workspace_root = Some(workspace.display().to_string());
            })
            .expect("seed orphan state");

        let released = cancel_orphaned_run(&store, "orphan-run").expect("orphan cancel");
        assert!(released.transitioned);
        assert!(released.had_recorded_processes);
        assert_eq!(
            released.killed_pgids,
            vec![decoy_pid],
            "matching provenance → killed"
        );
        assert!(released.stale_pgids.is_empty());
        assert!(released.stale_unsafe_pgids.is_empty());
        assert_eq!(released.workspace_action, OrphanedWorkspaceAction::Removed);
        assert!(
            !cleanup_root.exists(),
            "the recorded workspace cleanup root must be removed"
        );

        // The decoy is our direct child: reap it and confirm it was
        // SIGKILLed (a zombie would still answer kill(pid, 0), so the
        // wait() is the honest liveness proof here).
        let status = decoy.wait().expect("reap decoy");
        assert!(!status.success(), "decoy must have been killed");

        let state = store
            .load_state("orphan-run")
            .expect("load")
            .expect("state");
        assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
        assert_eq!(state.agents[0].outcome, Some(ReviewerOutcome::Cancelled));

        // Idempotent: already terminal — nothing further is touched.
        let again = cancel_orphaned_run(&store, "orphan-run").expect("second orphan cancel");
        assert!(!again.transitioned);
        assert!(again.killed_pgids.is_empty());
    }

    /// Pgid-reuse safety (codex R2 P1): a live process at a recorded
    /// pgid whose start-time provenance does NOT match (wrong ticks —
    /// the reused-pid case) or is missing (pre-provenance runs) must
    /// NEVER be killed; it is reported as `stale_unsafe_pgids`.
    #[cfg(unix)]
    #[tokio::test]
    async fn orphaned_run_cancel_refuses_unverified_live_pgids() {
        let (_dir, store, _repo) = test_store();
        store
            .create_run(
                "orphan-unsafe",
                &["wrong-ticks".to_string(), "no-ticks".to_string()],
                "sha",
                "scope",
            )
            .expect("create");

        let mut wrong = spawn_decoy_sleeper();
        let wrong_pid = wrong.id();
        let mut missing = spawn_decoy_sleeper();
        let missing_pid = missing.id();

        store
            .update_state("orphan-unsafe", |state| {
                // Wrong provenance: simulates a pid reused by an
                // unrelated process after the recorded reviewer died.
                state.agents[0].pid = Some(wrong_pid);
                state.agents[0].pgid = Some(wrong_pid);
                state.agents[0].proc_start_ticks = Some(1);
                // Missing provenance: an old run (or non-Linux record).
                state.agents[1].pid = Some(missing_pid);
                state.agents[1].pgid = Some(missing_pid);
                state.agents[1].proc_start_ticks = None;
            })
            .expect("seed unsafe state");

        let released = cancel_orphaned_run(&store, "orphan-unsafe").expect("orphan cancel");
        assert!(released.transitioned);
        assert!(released.had_recorded_processes);
        assert!(
            released.killed_pgids.is_empty(),
            "unverified live pgids must never be killed: {released:?}"
        );
        let mut unsafe_pgids = released.stale_unsafe_pgids.clone();
        unsafe_pgids.sort_unstable();
        let mut expected = vec![wrong_pid, missing_pid];
        expected.sort_unstable();
        assert_eq!(unsafe_pgids, expected);

        // Both decoys are still alive — kill + reap them ourselves.
        for (child, pid) in [(&mut wrong, wrong_pid), (&mut missing, missing_pid)] {
            assert!(
                child.try_wait().expect("try_wait decoy").is_none(),
                "decoy {pid} must still be running (engine refused the kill)"
            );
            child.kill().expect("kill decoy");
            child.wait().expect("reap decoy");
        }
    }

    /// A corrupted workspace_root must never delete arbitrary paths:
    /// the shape must match AND the path must canonicalize into the
    /// store's own task-worktree base — a same-named directory outside
    /// the base is refused; inside it is removed.
    #[test]
    fn orphaned_workspace_resolution_confines_to_the_task_worktree_base() {
        let (dir, store, _repo) = test_store();
        let base = task_worktree_base_for_store(&store).expect("base");

        // Inside the base: removable.
        let (inside_root, inside_workspace) = seed_confined_workspace(&store, "inside");
        match resolve_orphaned_workspace(&inside_workspace, Some(&base)) {
            OrphanedWorkspaceResolution::Remove(resolved) => {
                assert_eq!(
                    resolved,
                    std::fs::canonicalize(&inside_root).expect("canonicalize inside root")
                );
            }
            other => panic!("inside-base workspace must be removable, got {other:?}"),
        }

        // Same shape, same basename — but OUTSIDE the base: refused.
        let outside_root = dir.path().join("libra-task-worktree-outside");
        let outside_workspace = outside_root.join("workspace");
        std::fs::create_dir_all(&outside_workspace).expect("outside dir");
        assert_eq!(
            resolve_orphaned_workspace(&outside_workspace, Some(&base)),
            OrphanedWorkspaceResolution::Refused,
            "a same-named dir outside the base must be refused"
        );
        assert!(outside_root.exists());

        // Shape violations: refused regardless of location.
        for suspicious in ["/", "/home/user", "/etc/workspace"] {
            assert_eq!(
                resolve_orphaned_workspace(Path::new(suspicious), Some(&base)),
                OrphanedWorkspaceResolution::Refused,
                "{suspicious} must be refused"
            );
        }
        let inside_but_wrong_shape = base.join("not-a-task-worktree");
        std::fs::create_dir_all(&inside_but_wrong_shape).expect("wrong-shape dir");
        assert_eq!(
            resolve_orphaned_workspace(&inside_but_wrong_shape, Some(&base)),
            OrphanedWorkspaceResolution::Refused
        );

        // No base at all: nothing is removable.
        assert_eq!(
            resolve_orphaned_workspace(&inside_workspace, None),
            OrphanedWorkspaceResolution::Refused
        );

        // Already-gone leaf inside the base: attributable, nothing to do.
        let gone = base.join("libra-task-worktree-gone");
        assert_eq!(
            resolve_orphaned_workspace(&gone.join("workspace"), Some(&base)),
            OrphanedWorkspaceResolution::AlreadyGone(gone)
        );

        // A symlink inside the base pointing OUTSIDE it: refused (the
        // canonicalized target escapes the base).
        #[cfg(unix)]
        {
            let victim = dir.path().join("libra-task-worktree-victim");
            std::fs::create_dir_all(&victim).expect("victim dir");
            let link = base.join("libra-task-worktree-link");
            std::os::unix::fs::symlink(&victim, &link).expect("symlink");
            assert_eq!(
                resolve_orphaned_workspace(&link, Some(&base)),
                OrphanedWorkspaceResolution::Refused,
                "a symlink escaping the base must be refused"
            );
            assert!(victim.exists(), "the symlink target must not be touched");
        }

        // A symlink inside the base pointing at ANOTHER workspace inside
        // the base: the canonical target passes confinement, so only the
        // pre-canonicalization symlink refusal protects the victim
        // (codex A7 R3): removal through the link would delete the
        // victim's workspace, not the recorded run's.
        #[cfg(unix)]
        {
            let victim = base.join("libra-task-worktree-inside-victim");
            std::fs::create_dir_all(&victim).expect("inside victim dir");
            let link = base.join("libra-task-worktree-inside-link");
            std::os::unix::fs::symlink(&victim, &link).expect("inside symlink");
            assert_eq!(
                resolve_orphaned_workspace(&link, Some(&base)),
                OrphanedWorkspaceResolution::Refused,
                "an inside-base symlink to an inside-base victim must be refused"
            );
            assert!(
                victim.exists(),
                "the inside-base symlink target must not be touched"
            );
        }
    }
}
