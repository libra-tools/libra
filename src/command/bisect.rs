//! Binary-search regression hunting (`libra bisect`).
//!
//! Implements the full `bisect` subcommand family (`start`, `bad`, `good`,
//! `reset`, `skip`, `log`) by walking the commit graph between a known "good"
//! ancestor and a known "bad" descendant.
//!
//! Persistent state lives in a dedicated `bisect_state` table inside the
//! repository's SQLite store; the schema is owned by the versioned migration
//! `2026072301_bisect_state_worktree_scope` (one row per worktree,
//! `worktree_id` primary key — lazy DDL was retired with it).
//!
//! Non-obvious responsibilities:
//! - Detached vs. branch HEAD recovery: `start` records the original branch
//!   name (if any) so `reset` can re-attach instead of leaving the user in a
//!   detached state.
//! - Working-tree safety: every `bisect good/bad/skip` calls
//!   [`restore_to_commit`], which rewrites the per-worktree index AND the
//!   working tree from the target commit via the canonical restore contract
//!   (tracked paths only — untracked files survive a step, like `git bisect`).
//!   `start` still refuses to run with uncommitted changes that could be lost.
//! - Convergence semantics: [`BisectNext`] distinguishes between "more
//!   candidates", "single culprit found", and "all candidates skipped" so
//!   each handler can render the right user message.

use std::{
    collections::{HashSet, VecDeque},
    process::Command,
    str::FromStr,
};

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value};
use serde::Serialize;

use crate::{
    cli::Bisect,
    command::{
        load_object, restore,
        status::{changes_to_be_committed_safe, changes_to_be_staged_with_policy},
    },
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore::IgnorePolicy,
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// Bisect state stored in the repo database.
///
/// Persisted as a single row in the `bisect_state` SQLite table. Vector
/// fields (`good`, `skipped`) are serialised as JSON. `completed = true`
/// means the search converged; the row is preserved through completion so
/// `bisect reset` still has somewhere to read `orig_head` from.
#[derive(Debug, Clone)]
pub struct BisectState {
    /// Original HEAD commit before bisect started
    pub orig_head: ObjectHash,
    /// Original branch name (if on branch), None if detached
    pub orig_head_name: Option<String>,
    /// Bad commit hash (the commit with the bug)
    pub bad: Option<ObjectHash>,
    /// Good commit hashes (commits known to be working)
    pub good: Vec<ObjectHash>,
    /// Current test commit being checked
    pub current: Option<ObjectHash>,
    /// Skipped commits (marked with `bisect skip`)
    pub skipped: Vec<ObjectHash>,
    /// Estimated steps remaining
    pub steps: Option<usize>,
    /// Whether bisect has found the culprit (session ended but state preserved for reset)
    pub completed: bool,
    /// When true, follow only the first parent of merge commits while
    /// enumerating testable commits (`bisect start --first-parent`).
    pub first_parent: bool,
}

impl BisectState {
    /// Returns true when a non-completed bisect session row exists. Used by
    /// the bare-repo guard and by `start` to refuse re-entry.
    pub async fn is_in_progress() -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        Self::has_active_state_in_db(&db).await
    }

    /// Returns true when any row exists, including converged sessions whose
    /// state was preserved for `bisect reset`.
    pub async fn has_state() -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        Self::has_any_state_in_db(&db).await
    }

    /// Persist `self` as THIS worktree's single row in `bisect_state`.
    ///
    /// Boundary conditions:
    /// - Upserts on the `worktree_id` primary key in one statement, so
    ///   concurrent writers in the same worktree get last-writer-wins
    ///   semantics instead of one of them hitting a UNIQUE violation.
    pub async fn save(&self) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        Self::save_with_conn(&db, self).await
    }

    /// Load the persisted bisect state.
    ///
    /// Boundary conditions:
    /// - Returns `Err("No bisect in progress")` if the row is missing — this
    ///   propagates as a fatal CLI error in every caller.
    pub async fn load() -> Result<Self, String> {
        let db = get_db_conn_instance().await;
        Self::load_from_db(&db)
            .await?
            .ok_or_else(|| "No bisect in progress".to_string())
    }

    /// Drop the bisect state row. Idempotent on an empty table.
    pub async fn cleanup() -> Result<(), String> {
        let db = get_db_conn_instance().await;
        Self::clear_state_in_db(&db).await
    }

    /// This worktree's scope key for `bisect_state` (`""` = main worktree).
    fn scope_key() -> String {
        crate::internal::worktree_scope::WorktreeScope::current()
            .storage_key()
            .to_string()
    }

    /// Counts rows where `completed = 0`. Returns false on an empty table.
    async fn has_active_state_in_db<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
        // Check if there's an in-progress (not completed) bisect session in
        // THIS worktree's scope (Part C W1 §C.4.2).
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM bisect_state WHERE completed = 0 AND worktree_id = ?;",
            [Self::scope_key().into()],
        );

        if let Some(result) = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to query bisect_state: {e}"))?
        {
            let count: i64 = result.try_get_by_index(0).unwrap_or(0);
            return Ok(count > 0);
        }

        Ok(false)
    }

    /// Counts rows regardless of `completed`. Used by `reset` to recover even
    /// after the search converged.
    async fn has_any_state_in_db<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
        // Check if there's any bisect state (active or completed) in scope.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM bisect_state WHERE worktree_id = ?;",
            [Self::scope_key().into()],
        );

        if let Some(result) = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to query bisect_state: {e}"))?
        {
            let count: i64 = result.try_get_by_index(0).unwrap_or(0);
            return Ok(count > 0);
        }

        Ok(false)
    }

    /// Upsert `state` as this scope's `bisect_state` row (conflict target:
    /// the `worktree_id` primary key). Vector fields are JSON-encoded.
    async fn save_with_conn<C: ConnectionTrait>(db: &C, state: &BisectState) -> Result<(), String> {
        let good_json = serde_json::to_string(&state.good)
            .map_err(|e| format!("failed to serialize good commits: {e}"))?;
        let skipped_json = serde_json::to_string(&state.skipped)
            .map_err(|e| format!("failed to serialize skipped commits: {e}"))?;

        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
                INSERT INTO bisect_state (orig_head, orig_head_name, bad, good, current, skipped, steps, completed, first_parent, worktree_id)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(worktree_id) DO UPDATE SET
                    orig_head = excluded.orig_head,
                    orig_head_name = excluded.orig_head_name,
                    bad = excluded.bad,
                    good = excluded.good,
                    current = excluded.current,
                    skipped = excluded.skipped,
                    steps = excluded.steps,
                    completed = excluded.completed,
                    first_parent = excluded.first_parent;
            "#,
            [
                state.orig_head.to_string().into(),
                state
                    .orig_head_name
                    .clone()
                    .map(|s| s.into())
                    .unwrap_or(Value::String(None)),
                state
                    .bad
                    .map(|h| h.to_string().into())
                    .unwrap_or(Value::String(None)),
                good_json.into(),
                state
                    .current
                    .map(|h| h.to_string().into())
                    .unwrap_or(Value::String(None)),
                skipped_json.into(),
                state
                    .steps
                    .map(|s| s as i64)
                    .map(|v| v.into())
                    .unwrap_or(Value::BigInt(None)),
                (state.completed as i64).into(),
                (state.first_parent as i64).into(),
                Self::scope_key().into(),
            ],
        );

        db.execute(stmt)
            .await
            .map_err(|e| format!("failed to save bisect state: {e}"))?;

        Ok(())
    }

    /// Read the single bisect-state row, decoding the JSON-encoded
    /// `good`/`skipped` vectors and re-parsing each `ObjectHash`. Returns
    /// `Ok(None)` when no row exists.
    async fn load_from_db<C: ConnectionTrait>(db: &C) -> Result<Option<BisectState>, String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT orig_head, orig_head_name, bad, good, current, skipped, steps, completed, first_parent FROM bisect_state WHERE worktree_id = ? LIMIT 1;",
            [Self::scope_key().into()],
        );

        if let Some(result) = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to load bisect state: {e}"))?
        {
            let orig_head_str: String = result
                .try_get_by_index(0)
                .map_err(|e| format!("failed to read orig_head: {e}"))?;
            let orig_head_name: Option<String> = result.try_get_by_index(1).ok();
            let bad_str: Option<String> = result.try_get_by_index(2).ok();
            let good_json: String = result
                .try_get_by_index(3)
                .map_err(|e| format!("failed to read good: {e}"))?;
            let current_str: Option<String> = result.try_get_by_index(4).ok();
            let skipped_json: Option<String> = result.try_get_by_index(5).ok();
            let steps: Option<i64> = result.try_get_by_index(6).ok();
            let completed: i64 = result.try_get_by_index(7).unwrap_or(0);
            let first_parent: i64 = result.try_get_by_index(8).unwrap_or(0);

            let orig_head = ObjectHash::from_str(&orig_head_str)
                .map_err(|e| format!("invalid orig_head hash: {e}"))?;

            let bad = bad_str.and_then(|s| ObjectHash::from_str(&s).ok());

            let good: Vec<ObjectHash> = serde_json::from_str(&good_json)
                .map_err(|e| format!("failed to parse good commits: {e}"))?;

            let current = current_str.and_then(|s| ObjectHash::from_str(&s).ok());

            let skipped: Vec<ObjectHash> = skipped_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            return Ok(Some(BisectState {
                orig_head,
                orig_head_name,
                bad,
                good,
                current,
                skipped,
                steps: steps.map(|s| s as usize),
                completed: completed != 0,
                first_parent: first_parent != 0,
            }));
        }

        Ok(None)
    }

    /// Truncate THIS worktree's bisect-state row (Part C W1 §C.4.2). Used by
    /// `cleanup` after a session ends (`save` upserts in place instead). Never
    /// an unconditional `DELETE`, which would wipe another worktree's bisect.
    async fn clear_state_in_db<C: ConnectionTrait>(db: &C) -> Result<(), String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM bisect_state WHERE worktree_id = ?;",
            [Self::scope_key().into()],
        );

        db.execute(stmt)
            .await
            .map_err(|e| format!("failed to clear bisect state: {e}"))?;

        Ok(())
    }
}

/// `--help` examples shown in `libra bisect --help` output.
pub const BISECT_EXAMPLES: &str = "\
EXAMPLES:
    libra bisect start                         Begin a session; mark bad/good in subsequent steps
    libra bisect start <bad>                   Begin a session with the bad commit pre-marked
    libra bisect start <bad> --good <good>     Begin a session with both bounds pre-marked
    libra bisect start <bad> --first-parent    Bisect only the first-parent (mainline) history
    libra bisect bad                           Mark the current HEAD as bad
    libra bisect good                          Mark the current HEAD as good
    libra bisect skip                          Skip the current commit and continue
    libra bisect view                          Show current state and remaining candidates
    libra bisect visualize                     Alias for 'view' (text state; Libra has no gitk GUI)
    libra bisect run cargo test                Auto-bisect by running a test command
    libra bisect run cargo test -- --ignored   Forward flags to the test command
    libra bisect log                           Print full session log
    libra bisect reset                         End the session and restore HEAD";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum BisectOutput {
    Start {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        bad: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        good: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_bad: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
    },
    Mark {
        mark: String,
        commit: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_bad: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
    },
    Skip {
        commit: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
    },
    Reset {
        restored: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    Log {
        #[serde(skip_serializing_if = "Option::is_none")]
        bad: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        good: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
        completed: bool,
    },
    View {
        #[serde(skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        good: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bad: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        remaining: usize,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<String>,
        completed: bool,
    },
    Run {
        #[serde(skip_serializing_if = "Option::is_none")]
        first_bad: Option<String>,
        steps: usize,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining: Option<usize>,
    },
}

#[derive(Debug, thiserror::Error)]
enum BisectError {
    #[error("not in an active bisect; run `libra bisect start` first")]
    NotActive,
    #[error("bisect run requires bad and good bounds before automation can start")]
    RunBoundsMissing,
    #[error("bisect run command failed with non-recoverable exit code {exit_code}")]
    RunCommandFailed { exit_code: i32 },
    #[error("bisect run command terminated by signal")]
    RunCommandSignaled,
    #[error("no more candidate commits; bisect already converged")]
    NoMoreCandidates,
}

impl From<BisectError> for CliError {
    fn from(error: BisectError) -> Self {
        match error {
            BisectError::NotActive => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::BisectNotActive)
                .with_hint("start a bisect session before calling this subcommand"),
            BisectError::RunBoundsMissing => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("run 'libra bisect start <bad> --good <good>' before automation"),
            BisectError::RunCommandFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::BisectRunFailed)
                .with_hint("re-run with a script that returns 0 (good) / 1-127 (bad) / 125 (skip)"),
            BisectError::RunCommandSignaled => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::BisectRunFailed)
                .with_hint("ensure the script exits cleanly; signals abort the bisect"),
            BisectError::NoMoreCandidates => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::BisectNoCandidates)
                .with_hint("run `libra bisect view` to inspect remaining candidates"),
        }
    }
}

/// Entry point for the bisect command — dispatches the [`Bisect`] subvariant
/// to the matching `handle_*` function.
///
/// Boundary conditions:
/// - All variants forward their errors as `Err(CliError::fatal)` derived from
///   either DB failures, missing state, or rev-resolution failures.
pub async fn execute_safe(bisect_cmd: Bisect, output: &OutputConfig) -> CliResult<()> {
    // Part C W1 (§C.4.2): bisect is now safe in a LINKED worktree — its state
    // is the `bisect_state` row keyed by `worktree_id` (every load/save/clear
    // carries this worktree's scope key), its checkouts materialize into THIS
    // worktree's working directory, and `reset` restores only this worktree's
    // own HEAD. Two worktrees can bisect concurrently without interfering, so
    // the `ensure_main_worktree` guard is lifted here.
    let result = run_bisect(bisect_cmd).await?;
    render_bisect_output(&result, output)
}

async fn run_bisect(bisect_cmd: Bisect) -> CliResult<BisectOutput> {
    match bisect_cmd {
        Bisect::Start {
            bad,
            good,
            first_parent,
        } => run_bisect_start(bad, good, first_parent).await,
        Bisect::Bad { rev } => run_bisect_bad(rev).await,
        Bisect::Good { rev } => run_bisect_good(rev).await,
        Bisect::Reset { rev } => run_bisect_reset(rev).await,
        Bisect::Skip { rev } => run_bisect_skip(rev).await,
        Bisect::Log => run_bisect_log().await,
        Bisect::Run { cmd } => run_bisect_run(cmd).await,
        Bisect::View => run_bisect_view().await,
    }
}

fn render_bisect_output(result: &BisectOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("bisect", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    match result {
        BisectOutput::Start {
            status,
            current,
            first_bad,
            subject,
            remaining,
            ..
        } => {
            println!("Bisect session started");
            render_bisect_progress(status, current, first_bad, subject, *remaining);
        }
        BisectOutput::Mark {
            mark,
            commit,
            status,
            current,
            first_bad,
            subject,
            remaining,
            ..
        } => {
            println!("Marked {} as {mark}", short_hash_str(commit));
            render_bisect_progress(status, current, first_bad, subject, *remaining);
        }
        BisectOutput::Skip {
            commit,
            status,
            current,
            remaining,
            ..
        } => {
            println!("Skipped {}", short_hash_str(commit));
            render_bisect_progress(status, current, &None, &None, *remaining);
        }
        BisectOutput::Reset {
            restored,
            commit,
            branch,
        } => {
            if !restored {
                println!("No bisect in progress");
            } else if let Some(commit) = commit {
                if let Some(branch) = branch {
                    println!(
                        "HEAD is now at {} (on branch {})",
                        short_hash_str(commit),
                        branch
                    );
                } else {
                    println!("HEAD is now at {}", short_hash_str(commit));
                }
                println!(
                    "Bisect session ended, HEAD restored to {}",
                    short_hash_str(commit)
                );
            }
        }
        BisectOutput::Log {
            bad,
            good,
            current,
            skipped,
            steps,
            ..
        } => {
            println!("Bisect log:");
            println!(
                "  Bad: {}",
                bad.as_deref().map(short_hash_str).unwrap_or("not set")
            );
            let good = if good.is_empty() {
                String::new()
            } else {
                good.iter()
                    .map(|hash| short_hash_str(hash).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!("  Good: {good}");
            println!(
                "  Current: {}",
                current.as_deref().map(short_hash_str).unwrap_or("not set")
            );
            println!("  Skipped: {} commits", skipped.len());
            println!("  Steps remaining: {steps:?}");
        }
        BisectOutput::View {
            head,
            good,
            bad,
            remaining,
            skipped,
            ..
        } => {
            let good = good
                .first()
                .map(|hash| short_hash_str(hash).to_string())
                .unwrap_or_else(|| "(unset)".to_string());
            let bad = bad
                .as_deref()
                .map(short_hash_str)
                .unwrap_or("(unset)")
                .to_string();
            println!("Bisecting between {good} (good) and {bad} (bad)");
            println!(
                "HEAD: {}",
                head.as_deref().map(short_hash_str).unwrap_or("(none)")
            );
            println!("Remaining: {remaining} candidate(s)");
            if skipped.is_empty() {
                println!("Skipped: (none)");
            } else {
                let skipped = skipped
                    .iter()
                    .map(|hash| short_hash_str(hash).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("Skipped: {skipped}");
            }
        }
        BisectOutput::Run {
            first_bad,
            steps,
            skipped,
            ..
        } => {
            if let Some(first_bad) = first_bad {
                println!(
                    "Converged: first bad commit is {}",
                    short_hash_str(first_bad)
                );
            }
            println!("{steps} steps, {} skipped", skipped.len());
        }
    }

    Ok(())
}

fn render_bisect_progress(
    status: &str,
    current: &Option<String>,
    first_bad: &Option<String>,
    subject: &Option<String>,
    remaining: Option<usize>,
) {
    match status {
        "waiting_for_good" => println!("Status: waiting for good commit(s)"),
        "waiting_for_bad" => println!("Status: waiting for bad commit"),
        "testing" => {
            if let Some(current) = current {
                println!("HEAD is now at {}", short_hash_str(current));
            }
            if let Some(remaining) = remaining {
                println!("Bisecting: {remaining} revisions left to test after this");
            }
        }
        "converged" => {
            if let Some(first_bad) = first_bad {
                println!("{} is the first bad commit", short_hash_str(first_bad));
            }
            if let Some(subject) = subject {
                println!("{subject}");
            }
        }
        "all_skipped" => println!("Cannot narrow down further - all commits have been skipped"),
        _ => {}
    }
}

fn short_hash_str(hash: &str) -> &str {
    &hash[..hash.len().min(7)]
}

fn hash_to_string_opt(hash: Option<ObjectHash>) -> Option<String> {
    hash.map(|hash| hash.to_string())
}

fn hashes_to_strings(hashes: &[ObjectHash]) -> Vec<String> {
    hashes.iter().map(ToString::to_string).collect()
}

/// Read `core.bare` and decide whether the repository has a working tree.
///
/// Functional scope:
/// - Parses any of Git's boolean spellings (`true/yes/on/1`,
///   `false/no/off/0`) case-insensitively.
///
/// Boundary conditions:
/// - Missing config row -> `Ok(false)` (default to non-bare).
/// - Unparseable value -> fatal `CliError`. Fails closed: bisect cannot run
///   safely if we cannot tell whether a worktree exists.
/// - Underlying DB read failure -> fatal `CliError`.
async fn is_bare_repository() -> CliResult<bool> {
    fn parse_git_bool(value: &str) -> Option<bool> {
        match value.trim() {
            v if v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
                || v == "1" =>
            {
                Some(true)
            }
            v if v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("no")
                || v.eq_ignore_ascii_case("off")
                || v == "0" =>
            {
                Some(false)
            }
            _ => None,
        }
    }

    match ConfigKv::get("core.bare").await {
        Ok(Some(entry)) => parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "Invalid core.bare value: '{}'. Expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
        }),
        Ok(None) => Ok(false), // No config = not bare
        Err(e) => Err(CliError::fatal(format!(
            "Failed to read core.bare config: {e}"
        ))),
    }
}

/// Handle `bisect start` — initialise a new search session.
///
/// Functional scope:
/// - Refuses to run on bare repos and on dirty worktrees (staged or unstaged
///   changes, ignored files included — see [`IgnorePolicy::IncludeIgnored`]).
/// - Captures the original branch (or detached commit) and stores it inside
///   [`BisectState`] so `reset` can recover it.
/// - When both `bad` and `good` are supplied, validates the bounds via
///   [`find_next_bisect_point`] *before* persisting state to avoid orphaned
///   rows on invalid input.
/// - May immediately converge: a one-commit interval is reported as the
///   culprit and the session is marked `completed`.
///
/// Boundary conditions:
/// - Returns fatal errors with hints describing recovery (commit/stash,
///   `bisect reset`, etc.).
/// - Returns fatal error in an empty repository (no current commit).
///
/// See: tests::test_bisect_start_creates_state in
/// tests/command/bisect_test.rs:115;
/// tests::test_bisect_start_already_in_progress_fails in
/// tests/command/bisect_test.rs:370.
async fn run_bisect_start(
    bad: Option<String>,
    good: Option<String>,
    first_parent: bool,
) -> CliResult<BisectOutput> {
    // Bare repositories have no working tree - bisect requires checkout operations
    if is_bare_repository().await? {
        return Err(CliError::fatal("bisect cannot be run in a bare repository")
            .with_hint("bisect requires a working tree to check out commits for testing"));
    }

    // Require a clean working tree to prevent data loss: each bisect checkout
    // resets tracked paths in the index and working tree to the candidate
    // commit (uncommitted tracked changes would be lost), and a candidate that
    // TRACKS a path currently present as an untracked/ignored file would
    // overwrite it — so IncludeIgnored still catches .env/cache-dir collisions.
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to check staged changes: {e}")))?;
    let unstaged = changes_to_be_staged_with_policy(IgnorePolicy::IncludeIgnored)
        .map_err(|e| CliError::fatal(format!("Failed to check unstaged changes: {e}")))?;
    if !staged.is_empty() || !unstaged.is_empty() {
        return Err(CliError::fatal(
            "working tree contains uncommitted changes",
        )
        .with_hint("commit or stash your changes before running bisect. Note: each 'bisect good/bad/skip' step resets tracked files in the index and working tree to the candidate commit, so uncommitted tracked changes would be lost — and an untracked/ignored file would be overwritten if a candidate commit tracks a path with the same name"));
    }

    // Check if there's any existing bisect state (active or completed)
    // Must use has_state to prevent overwriting preserved orig_head from a completed session
    if BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(CliError::fatal(
            "a previous bisect session exists (completed or in progress); run 'bisect reset' first",
        ));
    }

    // Save original HEAD state
    let orig_head = Head::current_commit()
        .await
        .ok_or_else(|| CliError::fatal("Cannot start bisect in an empty repository"))?;

    let orig_head_name = match Head::current().await {
        Head::Branch(name) => Some(name),
        Head::Detached(_) => None,
    };

    // Parse optional bad and good commits
    let bad_hash = if let Some(bad_ref) = bad {
        Some(resolve_ref(&bad_ref).await?)
    } else {
        None
    };

    let good_hash = if let Some(good_ref) = good {
        Some(resolve_ref(&good_ref).await?)
    } else {
        None
    };

    let mut state = BisectState {
        orig_head,
        orig_head_name,
        bad: bad_hash,
        good: good_hash.map(|h| vec![h]).unwrap_or_default(),
        current: None,
        skipped: vec![],
        steps: None,
        completed: false,
        first_parent,
    };

    // If both bad and good are provided, validate bounds before saving state
    // This prevents leaving orphaned state if bounds are invalid
    if bad_hash.is_some() && good_hash.is_some() {
        // Validate that there are commits to test between bad and good
        if let Err(e) = find_next_bisect_point(&state).await {
            // Don't save state for invalid bounds - return error immediately
            return Err(CliError::fatal(e));
        }
    }

    state.save().await.map_err(CliError::fatal)?;

    if bad_hash.is_some() && good_hash.is_none() {
        return Ok(BisectOutput::Start {
            status: "waiting_for_good".to_string(),
            bad: hash_to_string_opt(state.bad),
            good: hashes_to_strings(&state.good),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
        });
    }

    // If good is provided but no bad, wait for bad
    if good_hash.is_some() && bad_hash.is_none() {
        return Ok(BisectOutput::Start {
            status: "waiting_for_bad".to_string(),
            bad: hash_to_string_opt(state.bad),
            good: hashes_to_strings(&state.good),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
        });
    }

    // If both bad and good are provided, find the first bisect point (already validated above)
    if bad_hash.is_some() && good_hash.is_some() {
        match find_next_bisect_point(&state)
            .await
            .map_err(CliError::fatal)?
        {
            BisectNext::Next(next) => {
                let remaining = checkout_to_bisect_point(next, &mut state).await?;
                return Ok(BisectOutput::Start {
                    status: "testing".to_string(),
                    bad: hash_to_string_opt(state.bad),
                    good: hashes_to_strings(&state.good),
                    current: hash_to_string_opt(state.current),
                    first_bad: None,
                    subject: None,
                    remaining,
                    steps: state.steps,
                });
            }
            BisectNext::Converged => {
                // Only one commit between bad and good - it's the culprit
                let bad_commit = state.bad.ok_or_else(|| CliError::fatal("No bad commit"))?;
                let commit = load_object::<Commit>(&bad_commit)
                    .map_err(|e| CliError::fatal(format!("Failed to load commit: {e}")))?;
                let subject = commit.message.lines().next().unwrap_or("");
                // Move HEAD to the culprit commit, mark completed but keep state for reset
                checkout_to_commit(bad_commit).await?;
                state.current = Some(bad_commit);
                state.completed = true;
                state.save().await.map_err(CliError::fatal)?;
                return Ok(BisectOutput::Start {
                    status: "converged".to_string(),
                    bad: hash_to_string_opt(state.bad),
                    good: hashes_to_strings(&state.good),
                    current: hash_to_string_opt(state.current),
                    first_bad: Some(bad_commit.to_string()),
                    subject: Some(subject.to_string()),
                    remaining: Some(0),
                    steps: state.steps,
                });
            }
            BisectNext::AllSkipped => {
                // This shouldn't happen on start since we haven't skipped anything yet
                // But handle gracefully just in case
                state.save().await.map_err(CliError::fatal)?;
                return Ok(BisectOutput::Start {
                    status: "all_skipped".to_string(),
                    bad: hash_to_string_opt(state.bad),
                    good: hashes_to_strings(&state.good),
                    current: hash_to_string_opt(state.current),
                    first_bad: None,
                    subject: None,
                    remaining: Some(0),
                    steps: state.steps,
                });
            }
        }
    }

    Ok(BisectOutput::Start {
        status: "started".to_string(),
        bad: hash_to_string_opt(state.bad),
        good: hashes_to_strings(&state.good),
        current: hash_to_string_opt(state.current),
        first_bad: None,
        subject: None,
        remaining: None,
        steps: state.steps,
    })
}

/// Handle `bisect bad` — mark `rev` (or HEAD) as containing the bug.
///
/// Functional scope:
/// - Loads existing state, refuses to run on a completed session, then
///   updates `state.bad` and either waits for a good commit, advances to the
///   next test point, or converges.
///
/// Boundary conditions:
/// - Resolution failures return fatal errors via [`resolve_ref`].
/// - When `state.good` is empty the session simply records the bad commit
///   and prints "waiting for good commit(s)".
///
/// See: tests::test_bisect_mark_bad_then_good in
/// tests/command/bisect_test.rs:172;
/// tests::test_bisect_find_first_bad_commit in
/// tests/command/bisect_test.rs:211.
async fn run_bisect_bad(rev: Option<String>) -> CliResult<BisectOutput> {
    let mut state = BisectState::load().await.map_err(CliError::fatal)?;

    // Block operations on completed sessions - user must reset first
    if state.completed {
        return Err(
            CliError::fatal("bisect session has already found the culprit")
                .with_hint("run 'bisect reset' to end the session and restore your original HEAD"),
        );
    }

    let bad_hash = if let Some(rev) = rev {
        resolve_ref(&rev).await?
    } else {
        Head::current_commit()
            .await
            .ok_or_else(|| CliError::fatal("Cannot mark HEAD as bad - no current commit"))?
    };

    state.bad = Some(bad_hash);

    // Check if we have both good and bad
    if state.good.is_empty() {
        // No good commits yet - just save and wait for good
        state.save().await.map_err(CliError::fatal)?;
        return Ok(BisectOutput::Mark {
            mark: "bad".to_string(),
            commit: bad_hash.to_string(),
            status: "waiting_for_good".to_string(),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
        });
    }

    // Validate bounds before printing mark message (ensures output matches actual state)
    match find_next_bisect_point(&state)
        .await
        .map_err(CliError::fatal)?
    {
        BisectNext::Next(next) => {
            let remaining = checkout_to_bisect_point(next, &mut state).await?;
            Ok(BisectOutput::Mark {
                mark: "bad".to_string(),
                commit: bad_hash.to_string(),
                status: "testing".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining,
                steps: state.steps,
            })
        }
        BisectNext::Converged => {
            // We found the culprit!
            let bad = state
                .bad
                .ok_or_else(|| CliError::fatal("No bad commit set"))?;
            let commit = load_object::<Commit>(&bad)
                .map_err(|e| CliError::fatal(format!("Failed to load commit: {e}")))?;
            let subject = commit.message.lines().next().unwrap_or("");
            // Move HEAD to the culprit commit, mark completed but keep state for reset
            checkout_to_commit(bad).await?;
            state.current = Some(bad);
            state.completed = true;
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: "bad".to_string(),
                commit: bad_hash.to_string(),
                status: "converged".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: Some(bad.to_string()),
                subject: Some(subject.to_string()),
                remaining: Some(0),
                steps: state.steps,
            })
        }
        BisectNext::AllSkipped => {
            // Bounds valid but all candidates skipped - mark is saved
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: "bad".to_string(),
                commit: bad_hash.to_string(),
                status: "all_skipped".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining: Some(0),
                steps: state.steps,
            })
        }
    }
}

/// Handle `bisect good` — push `rev` (or HEAD) onto the known-good list.
///
/// Functional scope:
/// - Mirror image of [`handle_bad`]: appends to `state.good` and either waits
///   for a bad commit, advances to the next test point, or converges.
///
/// Boundary conditions:
/// - Refuses to run on a completed session (the user must `bisect reset`).
async fn run_bisect_good(rev: Option<String>) -> CliResult<BisectOutput> {
    let mut state = BisectState::load().await.map_err(CliError::fatal)?;

    // Block operations on completed sessions - user must reset first
    if state.completed {
        return Err(
            CliError::fatal("bisect session has already found the culprit")
                .with_hint("run 'bisect reset' to end the session and restore your original HEAD"),
        );
    }

    let good_hash = if let Some(rev) = rev {
        resolve_ref(&rev).await?
    } else {
        Head::current_commit()
            .await
            .ok_or_else(|| CliError::fatal("Cannot mark HEAD as good - no current commit"))?
    };

    state.good.push(good_hash);

    // Check if we have a bad commit
    if state.bad.is_none() {
        // No bad commit yet - just save and wait for bad
        state.save().await.map_err(CliError::fatal)?;
        return Ok(BisectOutput::Mark {
            mark: "good".to_string(),
            commit: good_hash.to_string(),
            status: "waiting_for_bad".to_string(),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
        });
    }

    // Validate bounds before printing mark message (ensures output matches actual state)
    match find_next_bisect_point(&state)
        .await
        .map_err(CliError::fatal)?
    {
        BisectNext::Next(next) => {
            let remaining = checkout_to_bisect_point(next, &mut state).await?;
            Ok(BisectOutput::Mark {
                mark: "good".to_string(),
                commit: good_hash.to_string(),
                status: "testing".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining,
                steps: state.steps,
            })
        }
        BisectNext::Converged => {
            // We found the culprit!
            let bad = state
                .bad
                .ok_or_else(|| CliError::fatal("No bad commit set"))?;
            let commit = load_object::<Commit>(&bad)
                .map_err(|e| CliError::fatal(format!("Failed to load commit: {e}")))?;
            let subject = commit.message.lines().next().unwrap_or("");
            // Move HEAD to the culprit commit, mark completed but keep state for reset
            checkout_to_commit(bad).await?;
            state.current = Some(bad);
            state.completed = true;
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: "good".to_string(),
                commit: good_hash.to_string(),
                status: "converged".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: Some(bad.to_string()),
                subject: Some(subject.to_string()),
                remaining: Some(0),
                steps: state.steps,
            })
        }
        BisectNext::AllSkipped => {
            // Bounds valid but all candidates skipped - mark is saved
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: "good".to_string(),
                commit: good_hash.to_string(),
                status: "all_skipped".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining: Some(0),
                steps: state.steps,
            })
        }
    }
}

/// Handle `bisect reset` — terminate the session and restore HEAD.
///
/// Functional scope:
/// - With an explicit `rev`, jumps HEAD to that commit (detached).
/// - With the original branch still available, re-attaches HEAD to it via
///   [`restore_to_branch`]; otherwise falls back to a detached checkout of
///   the original commit.
/// - Always cleans up the bisect-state row last.
///
/// Boundary conditions:
/// - With no state row at all, prints "No bisect in progress" and returns
///   `Ok(())` — `reset` is the supported escape hatch for stale state.
///
/// See: tests::test_bisect_reset in tests/command/bisect_test.rs:264.
async fn run_bisect_reset(rev: Option<String>) -> CliResult<BisectOutput> {
    // Use has_state to check if there's any bisect state (active or completed)
    let has_state = BisectState::has_state().await.map_err(CliError::fatal)?;

    if !has_state {
        return Ok(BisectOutput::Reset {
            restored: false,
            commit: None,
            branch: None,
        });
    }

    let state = BisectState::load().await.map_err(CliError::fatal)?;

    // Determine where to reset
    let (target_hash, target_branch) = if let Some(rev) = rev {
        (resolve_ref(&rev).await?, None)
    } else if let Some(ref branch_name) = state.orig_head_name {
        // Restore to original branch - use its current commit (branch may have moved during bisect)
        match Branch::find_branch_result(branch_name, None)
            .await
            .map_err(|error| map_bisect_branch_store_error(branch_name, error))?
        {
            Some(branch) => (branch.commit, Some(branch_name.clone())),
            None => {
                // Branch no longer exists - fall back to orig_head commit
                (state.orig_head, None)
            }
        }
    } else {
        (state.orig_head, None)
    };

    // Part C W1 (§C.4.2): branches are SHARED across worktrees. While this
    // worktree bisected (detached), another worktree may have legitimately
    // checked out the original branch — re-attaching would put one branch on
    // two HEADs, the exact state `switch`/`checkout` categorically refuse.
    // Warn and end the session detached at the target commit instead.
    let target_branch = match target_branch {
        Some(branch_name) => {
            if let Some(other) = Head::branch_checked_out_elsewhere(&branch_name).await {
                crate::utils::error::emit_warning(format!(
                    "not re-attaching branch '{branch_name}': it is now checked out at \
                     worktree '{other}'; bisect ends detached at {target_hash} \
                     (use 'libra switch' to pick a branch)"
                ));
                None
            } else {
                Some(branch_name)
            }
        }
        None => None,
    };

    // Restore original HEAD - use branch if available to avoid detached state
    if let Some(branch_name) = target_branch.clone() {
        restore_to_branch(branch_name, target_hash).await?;
    } else {
        checkout_to_commit(target_hash).await?;
    }

    // Clean up bisect state
    BisectState::cleanup().await.map_err(CliError::fatal)?;

    Ok(BisectOutput::Reset {
        restored: true,
        commit: Some(target_hash.to_string()),
        branch: target_branch,
    })
}

fn map_bisect_branch_store_error(branch_name: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': failed to query branch storage: {detail}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("repair branch storage or reset to an explicit revision with 'libra bisect reset <rev>'."),
        BranchStoreError::Corrupt { .. } => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': {error}"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
        .with_hint("repair branch storage or reset to an explicit revision with 'libra bisect reset <rev>'."),
        BranchStoreError::NotFound(_) => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': {error}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        BranchStoreError::Delete { .. } => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': {error}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed),
    }
}

/// Re-attach HEAD to `branch_name` and restore the worktree to `commit_hash`.
///
/// Functional scope:
/// - Updates HEAD inside a transaction, then verifies the update visibly
///   succeeded because [`Head::update_with_conn`] swallows write errors and
///   only logs them.
/// - Calls [`restore_to_commit`] to populate the worktree from the target
///   tree.
///
/// Boundary conditions:
/// - Transaction begin/commit failures and HEAD-mismatch detection both
///   return fatal `CliError`s with diagnostic messages.
async fn restore_to_branch(branch_name: String, commit_hash: ObjectHash) -> CliResult<()> {
    let db = get_db_conn_instance().await;

    let txn = db
        .begin()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to begin transaction: {e}")))?;

    // Update HEAD to point to the branch
    let new_head = Head::Branch(branch_name.clone());
    Head::update_with_conn(&txn, new_head.clone(), None).await;

    txn.commit()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to commit transaction: {e}")))?;

    // Verify HEAD was updated correctly (update_with_conn logs errors but doesn't return Result)
    let actual_head = Head::current().await;
    if !matches!(actual_head, Head::Branch(ref name) if name == &branch_name) {
        return Err(CliError::fatal(format!(
            "Failed to update HEAD to branch '{}'",
            branch_name
        )));
    }

    // Restore working directory to the commit's tree
    restore_to_commit(commit_hash).await?;
    Ok(())
}

/// Handle `bisect skip` — exclude the current (or named) commit from
/// further candidates.
///
/// Functional scope:
/// - Pushes the commit to `state.skipped` and re-runs the search. The set of
///   skipped commits is consulted by [`get_testable_commits`].
///
/// Boundary conditions:
/// - Refuses to run on a completed session.
/// - Returns `BisectNext::AllSkipped` when every remaining candidate has been
///   skipped — the search reports the deadlock and saves state for `reset`.
///
/// See: tests::test_bisect_skip in tests/command/bisect_test.rs:309.
async fn run_bisect_skip(rev: Option<String>) -> CliResult<BisectOutput> {
    let mut state = BisectState::load().await.map_err(CliError::fatal)?;

    // Block operations on completed sessions - user must reset first
    if state.completed {
        return Err(
            CliError::fatal("bisect session has already found the culprit")
                .with_hint("run 'bisect reset' to end the session and restore your original HEAD"),
        );
    }

    let skip_hash = if let Some(rev) = rev {
        resolve_ref(&rev).await?
    } else {
        state
            .current
            .ok_or_else(|| CliError::fatal("No current commit to skip"))?
    };

    state.skipped.push(skip_hash);

    // Find next bisect point
    match find_next_bisect_point(&state)
        .await
        .map_err(CliError::fatal)?
    {
        BisectNext::Next(next) => {
            let remaining = checkout_to_bisect_point(next, &mut state).await?;
            Ok(BisectOutput::Skip {
                commit: skip_hash.to_string(),
                status: "testing".to_string(),
                current: hash_to_string_opt(state.current),
                remaining,
                steps: state.steps,
            })
        }
        BisectNext::Converged => {
            // Should not happen in skip - no single culprit when skipping
            // But handle gracefully if all but one were skipped
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Skip {
                commit: skip_hash.to_string(),
                status: "converged".to_string(),
                current: hash_to_string_opt(state.current),
                remaining: Some(1),
                steps: state.steps,
            })
        }
        BisectNext::AllSkipped => {
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Skip {
                commit: skip_hash.to_string(),
                status: "all_skipped".to_string(),
                current: hash_to_string_opt(state.current),
                remaining: Some(0),
                steps: state.steps,
            })
        }
    }
}

/// Handle `bisect log` — print a human-readable status of the active session.
///
/// Boundary conditions:
/// - Returns the underlying load error when no session row exists; this is
///   the only handler that intentionally lacks a "no-state" early return.
///
/// See: tests::test_bisect_log in tests/command/bisect_test.rs:347.
async fn run_bisect_log() -> CliResult<BisectOutput> {
    let state = BisectState::load().await.map_err(CliError::fatal)?;

    Ok(BisectOutput::Log {
        bad: hash_to_string_opt(state.bad),
        good: hashes_to_strings(&state.good),
        current: hash_to_string_opt(state.current),
        skipped: hashes_to_strings(&state.skipped),
        steps: state.steps,
        completed: state.completed,
    })
}

/// Handle `bisect view` — print the current bisect state in the same shape
/// as the JSON output described in the C4 plan.
///
/// Behavior:
/// - When no session row exists, returns a fatal `RepoStateInvalid` error
///   (the user must `bisect start` first).
/// - When a session is in progress (or completed), prints the current HEAD,
///   good/bad bounds, remaining-candidate count, and skipped commits.
async fn run_bisect_view() -> CliResult<BisectOutput> {
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }

    let state = BisectState::load().await.map_err(CliError::fatal)?;
    let head = Head::current_commit().await.map(|h| h.to_string());
    let remaining = count_commits_to_test(&state).await.unwrap_or(0);

    Ok(BisectOutput::View {
        head,
        good: hashes_to_strings(&state.good),
        bad: hash_to_string_opt(state.bad),
        current: hash_to_string_opt(state.current),
        remaining,
        skipped: hashes_to_strings(&state.skipped),
        completed: state.completed,
    })
}

/// Handle `bisect run <cmd> [args...]` — execute the script for each commit
/// until the search converges (or every candidate has been skipped).
///
/// Exit-code semantics (aligned with `git bisect run`):
/// - `0`            → mark good
/// - `125`          → mark skip (cannot test this commit)
/// - `1..=127`      → mark bad
/// - `128..`        → fatal: terminate the bisect with `BISECT_RUN_FAILED`
/// - signal / `None`→ fatal: same as 128+
async fn run_bisect_run(cmd: Vec<String>) -> CliResult<BisectOutput> {
    if !BisectState::is_in_progress()
        .await
        .map_err(CliError::fatal)?
    {
        return Err(BisectError::NotActive.into());
    }

    let initial_state = BisectState::load().await.map_err(CliError::fatal)?;
    if initial_state.bad.is_none()
        || initial_state.good.is_empty()
        || initial_state.current.is_none()
    {
        return Err(BisectError::RunBoundsMissing.into());
    }

    let (executable, args) = cmd
        .split_first()
        .ok_or_else(|| CliError::fatal("`bisect run` requires a command to execute"))?;

    let mut steps = 0usize;
    let mut session_skipped: Vec<String> = Vec::new();

    loop {
        let state = BisectState::load().await.map_err(CliError::fatal)?;
        if state.completed {
            // Already converged before we got to act this iteration.
            return Ok(BisectOutput::Run {
                first_bad: hash_to_string_opt(state.bad),
                steps,
                skipped: session_skipped,
                remaining: Some(0),
            });
        }

        let head_short = Head::current_commit()
            .await
            .map(|h| h.to_string()[..7].to_string())
            .unwrap_or_else(|| "(no HEAD)".to_string());

        let status = Command::new(executable).args(args).status().map_err(|e| {
            CliError::fatal(format!("failed to spawn `{executable}`: {e}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

        match status.code() {
            Some(0) => {
                steps += 1;
                run_bisect_good(None).await?;
            }
            Some(125) => {
                steps += 1;
                session_skipped.push(head_short.clone());
                run_bisect_skip(None).await?;
            }
            Some(code) if (1..=127).contains(&code) => {
                steps += 1;
                run_bisect_bad(None).await?;
            }
            Some(code) => {
                return Err(BisectError::RunCommandFailed { exit_code: code }.into());
            }
            None => {
                return Err(BisectError::RunCommandSignaled.into());
            }
        }

        // After the mark, check if the session converged or all candidates skipped.
        let next_state = BisectState::load().await.map_err(CliError::fatal)?;
        if next_state.completed {
            return Ok(BisectOutput::Run {
                first_bad: hash_to_string_opt(next_state.bad),
                steps,
                skipped: session_skipped,
                remaining: Some(0),
            });
        }

        let remaining = count_commits_to_test(&next_state).await.unwrap_or(0);
        if remaining == 0 {
            return Err(BisectError::NoMoreCandidates.into());
        }
    }
}

/// Convert a user-supplied revision string (branch, tag, hash) into the
/// concrete commit it points at. Errors out fatally with the underlying
/// resolver message if it cannot be resolved.
async fn resolve_ref(ref_str: &str) -> CliResult<ObjectHash> {
    util::get_commit_base(ref_str)
        .await
        .map_err(|e| CliError::fatal(format!("Cannot resolve '{}': {}", ref_str, e)))
}

/// Move HEAD to `commit_hash` in detached mode and lay the matching tree on
/// the worktree.
///
/// Boundary conditions:
/// - The HEAD update happens inside a SQLite transaction. The worktree
///   restore runs after commit, so a partial failure between them leaves the
///   worktree out of sync until `bisect reset`.
async fn checkout_to_commit(commit_hash: ObjectHash) -> CliResult<()> {
    let db = get_db_conn_instance().await;

    let txn = db
        .begin()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to begin transaction: {e}")))?;

    let new_head = Head::Detached(commit_hash);
    Head::update_with_conn(&txn, new_head, None).await;

    txn.commit()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to commit transaction: {e}")))?;

    // Restore working directory
    restore_to_commit(commit_hash).await?;
    Ok(())
}

/// Checkout to a candidate commit, record it as `state.current`, and update
/// the steps-remaining estimate.
///
/// Boundary conditions:
/// - The "remaining steps" count is only updated when `state.bad` is set;
///   otherwise the user is still in the bounds-collection phase.
async fn checkout_to_bisect_point(
    commit_hash: ObjectHash,
    state: &mut BisectState,
) -> CliResult<Option<usize>> {
    checkout_to_commit(commit_hash).await?;

    state.current = Some(commit_hash);

    // Calculate remaining steps
    if state.bad.is_some() {
        let remaining = count_commits_to_test(state)
            .await
            .map_err(CliError::fatal)?;
        state.steps = Some(remaining);
    }

    state.save().await.map_err(CliError::fatal)?;

    Ok(state.steps)
}

/// Repaint the index AND the worktree from the tree of `commit_hash`, via the
/// canonical restore contract (the same call `switch` makes when moving HEAD).
///
/// Functional scope:
/// - `restore --source <commit> --staged --worktree <workdir>` rewrites the
///   per-worktree index and working tree together: tracked paths are laid from
///   the target tree (LFS pointers and symlinks honoured), and index-tracked
///   paths absent from the target are removed (no-overlay).
///
/// Boundary conditions (Part C W1, §C.4.2):
/// - `restore` resolves the index and working directory PER-WORKTREE, so a
///   linked worktree's bisect checkouts materialize into its own tree. The
///   former burn-down repaint resolved the MAIN worktree's directory via the
///   shared storage path's parent and never rewrote the index, so every step
///   showed phantom `status` modifications.
/// - Untracked files created mid-session (e.g. by a `bisect run` script) now
///   survive a step, matching `git bisect`'s checkout semantics; `start` still
///   requires a clean tree so nothing pre-existing can be clobbered.
async fn restore_to_commit(commit_hash: ObjectHash) -> CliResult<()> {
    let restore_args = restore::RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        worktree: true,
        staged: true,
        source: Some(commit_hash.to_string()),
        pathspec: vec![util::working_dir_string()],
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };
    restore::execute_to_output(restore_args).await?;
    Ok(())
}

/// Result of one bisect-search iteration.
///
/// Drives caller branching: keep going (`Next`), report the culprit
/// (`Converged`), or surface the deadlock (`AllSkipped`).
enum BisectNext {
    /// There are more commits to test - return the next midpoint
    Next(ObjectHash),
    /// Only one candidate remains - it's the culprit (first bad commit)
    Converged,
    /// All remaining candidates were skipped - cannot determine culprit
    AllSkipped,
}

/// Compute the next commit to check out under binary search.
///
/// Functional scope:
/// - Calls [`get_testable_commits`] to enumerate descendants of `bad` that
///   are not also ancestors of any `good` commit and have not been skipped.
/// - Returns `Converged` when only one candidate remains, `AllSkipped` when
///   the list is empty due to skips, or `Next(midpoint)` otherwise.
///
/// Boundary conditions:
/// - Returns an error string when state has no `bad` or no `good` set, or
///   when the bounds are inconsistent (no commits between them) — the
///   caller surfaces this as a fatal CLI error.
async fn find_next_bisect_point(state: &BisectState) -> Result<BisectNext, String> {
    let bad = state.bad.ok_or("No bad commit set")?;

    if state.good.is_empty() {
        return Err("No good commits set".to_string());
    }

    // Get all ancestors of bad that are not ancestors of any good commit
    let testable =
        get_testable_commits(&bad, &state.good, &state.skipped, state.first_parent).await?;

    if testable.is_empty() {
        // Check if this is because all candidates were skipped
        // If there are no skipped commits, it's invalid bounds (bad is ancestor of good)
        // If there are skipped commits, check if unskipped testable would be non-empty
        if state.skipped.is_empty() {
            // No skipped commits but empty testable = invalid bounds
            return Err(
                "No commits left to test between good and bad bounds - check that good and bad commits have a valid ancestor relationship".to_string()
            );
        }
        // There are skipped commits - check if without skip filter we'd have candidates
        let testable_without_skip =
            get_testable_commits(&bad, &state.good, &[], state.first_parent).await?;
        if testable_without_skip.is_empty() {
            // Still empty without skip filter = invalid bounds
            return Err(
                "No commits left to test between good and bad bounds - check that good and bad commits have a valid ancestor relationship".to_string()
            );
        }
        // Would have candidates without skip = all candidates were skipped
        return Ok(BisectNext::AllSkipped);
    }

    // If only one commit is testable, it's the first bad commit (convergence)
    if testable.len() == 1 {
        return Ok(BisectNext::Converged);
    }

    // Find the middle commit (prefer earlier commits to narrow down faster)
    // testable is sorted oldest first, so we pick the middle index
    let mid = (testable.len() - 1) / 2;
    Ok(BisectNext::Next(testable[mid]))
}

/// Enumerate the candidate commits between `bad` and any `good`.
///
/// Functional scope:
/// - Builds the set of ancestors of every `good` commit, then BFS-walks
///   from `bad` skipping anything in that set or in `skipped`.
/// - Returns the candidates in oldest-first order, suitable for
///   midpoint selection by [`find_next_bisect_point`].
///
/// Boundary conditions:
/// - Returns an empty vector when the bounds are inconsistent or all
///   candidates have been skipped; callers must distinguish the two cases.
/// - Each commit object load is fatal on I/O / corruption errors.
async fn get_testable_commits(
    bad: &ObjectHash,
    good: &[ObjectHash],
    skipped: &[ObjectHash],
    first_parent: bool,
) -> Result<Vec<ObjectHash>, String> {
    // Build set of good ancestors
    let good_ancestors: HashSet<ObjectHash> = get_all_ancestors(good).await?;

    // Build set of skipped commits
    let skipped_set: HashSet<ObjectHash> = skipped.iter().copied().collect();

    // BFS from bad, collecting commits not in good_ancestors or skipped
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut testable = Vec::new();

    queue.push_back(*bad);

    while let Some(commit_hash) = queue.pop_front() {
        if visited.contains(&commit_hash) {
            continue;
        }
        visited.insert(commit_hash);

        // Skip if this is a good ancestor
        if good_ancestors.contains(&commit_hash) {
            continue;
        }

        // Skip if explicitly marked as skipped
        if skipped_set.contains(&commit_hash) {
            continue;
        }

        let commit = load_object::<Commit>(&commit_hash)
            .map_err(|e| format!("Failed to load commit {}: {}", commit_hash, e))?;

        // Add to testable list
        testable.push(commit_hash);

        // Add parents to queue. With `--first-parent`, follow only the first
        // parent of merge commits so bisecting stays on the mainline history.
        if first_parent {
            if let Some(parent) = commit.parent_commit_ids.first() {
                queue.push_back(*parent);
            }
        } else {
            for parent in &commit.parent_commit_ids {
                queue.push_back(*parent);
            }
        }
    }

    // Sort by commit order (oldest first for proper bisect ordering)
    // We reverse the order since BFS gives us newest first
    testable.reverse();

    Ok(testable)
}

/// Collect every transitive parent of the input commits into one set.
///
/// Boundary conditions:
/// - Each commit appears in the result; the search terminates naturally on
///   root commits whose `parent_commit_ids` is empty.
async fn get_all_ancestors(commits: &[ObjectHash]) -> Result<HashSet<ObjectHash>, String> {
    let mut ancestors = HashSet::new();
    let mut queue = VecDeque::new();

    for commit in commits {
        queue.push_back(*commit);
    }

    while let Some(commit_hash) = queue.pop_front() {
        if ancestors.contains(&commit_hash) {
            continue;
        }
        ancestors.insert(commit_hash);

        let commit = load_object::<Commit>(&commit_hash)
            .map_err(|e| format!("Failed to load commit {}: {}", commit_hash, e))?;

        for parent in &commit.parent_commit_ids {
            queue.push_back(*parent);
        }
    }

    Ok(ancestors)
}

/// Length of the candidate list — i.e. the number of binary-search steps
/// remaining before convergence. Used purely for the user-visible
/// "X revisions left to test" message.
async fn count_commits_to_test(state: &BisectState) -> Result<usize, String> {
    let bad = state.bad.ok_or("No bad commit set")?;

    if state.good.is_empty() {
        return Err("No good commits set".to_string());
    }

    let testable =
        get_testable_commits(&bad, &state.good, &state.skipped, state.first_parent).await?;
    Ok(testable.len())
}
