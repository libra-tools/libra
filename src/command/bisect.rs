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
//!   `restore_to_commit`, which rewrites the per-worktree index AND the
//!   working tree from the target commit via the canonical restore contract
//!   (tracked paths only — untracked files survive a step, like `git bisect`).
//!   `start` still refuses to run with uncommitted changes that could be lost.
//! - Convergence semantics: `BisectNext` distinguishes between "more
//!   candidates", "single culprit found", and "all candidates skipped" so
//!   each handler can render the right user message.

use std::{
    collections::{HashSet, VecDeque},
    process::Command,
};

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};
use serde::Serialize;

use crate::{
    cli::Bisect,
    command::{
        load_object, restore,
        status::{Changes, changes_to_be_committed_safe, changes_to_be_staged},
    },
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
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
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::has_active_state_in_db(&db).await
    }

    /// Returns true when any row exists, including converged sessions whose
    /// state was preserved for `bisect reset`.
    pub async fn has_state() -> Result<bool, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::has_any_state_in_db(&db).await
    }

    /// Persist `self` as THIS worktree's single row in `bisect_state`.
    ///
    /// Boundary conditions:
    /// - Upserts on the `worktree_id` primary key in one statement, so
    ///   concurrent writers in the same worktree get last-writer-wins
    ///   semantics instead of one of them hitting a UNIQUE violation.
    ///   Last-writer-wins is right for an OWNER advancing its own session; a
    ///   STARTING session must use [`BisectState::claim_start`] instead.
    pub async fn save(&self) -> Result<(), String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::save_with_conn(&db, self).await
    }

    /// Claim this worktree's bisect slot for a STARTING session (§C.4.4).
    ///
    /// `ensure_none_for_bisect` is a check, and a check followed by an upsert
    /// is a TOCTOU window: two `bisect start` runs in one worktree both see no
    /// row, both check out a candidate, and the loser's upsert overwrites the
    /// winner's `orig_head` — so `bisect reset` afterwards returns HEAD to the
    /// wrong place, which is the one thing a bisect must not get wrong. A bare
    /// `INSERT` against `worktree_id PRIMARY KEY` lets exactly one starter
    /// win; the loser gets the ordinary "already in progress" refusal.
    pub async fn claim_start(&self) -> Result<(), String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        let good_json = serde_json::to_string(&self.good)
            .map_err(|e| format!("failed to serialize good commits: {e}"))?;
        let skipped_json = serde_json::to_string(&self.skipped)
            .map_err(|e| format!("failed to serialize skipped commits: {e}"))?;
        let scope_key = crate::internal::worktree_scope::WorktreeScope::for_request()
            .storage_key()
            .to_string();
        let result = db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO bisect_state (orig_head, orig_head_name, bad, good, current, \
                 skipped, steps, completed, first_parent, worktree_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    self.orig_head.to_string().into(),
                    self.orig_head_name
                        .clone()
                        .map(|name| name.into())
                        .unwrap_or(Value::String(None)),
                    self.bad
                        .map(|h| h.to_string().into())
                        .unwrap_or(Value::String(None)),
                    good_json.into(),
                    self.current
                        .map(|h| h.to_string().into())
                        .unwrap_or(Value::String(None)),
                    skipped_json.into(),
                    self.steps
                        .map(|s| s as i64)
                        .map(|v| v.into())
                        .unwrap_or(Value::BigInt(None)),
                    (self.completed as i64).into(),
                    (self.first_parent as i64).into(),
                    scope_key.into(),
                ],
            ))
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(err) if crate::internal::sequencer::is_unique_violation(&err) => {
                Err("a bisect is already in progress in this worktree".to_string())
            }
            Err(err) => Err(format!("failed to claim bisect_state: {err}")),
        }
    }

    /// Load the persisted bisect state.
    ///
    /// Boundary conditions:
    /// - Returns `Err("No bisect in progress")` if the row is missing — this
    ///   propagates as a fatal CLI error in every caller.
    pub async fn load() -> Result<Self, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::load_from_db(&db)
            .await?
            .ok_or_else(|| "No bisect in progress".to_string())
    }

    /// Drop the bisect state row. Idempotent on an empty table.
    pub async fn cleanup() -> Result<(), String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::clear_state_in_db(&db).await
    }

    /// This worktree's scope key for `bisect_state` (`""` = main worktree).
    fn scope_key() -> String {
        crate::internal::worktree_scope::WorktreeScope::for_request()
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
            .query_one_raw(stmt)
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
            .query_one_raw(stmt)
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

        db.execute_raw(stmt)
            .await
            .map_err(|e| format!("failed to save bisect state: {e}"))?;

        Ok(())
    }

    /// Read the single bisect-state row, decoding the JSON-encoded
    /// `good`/`skipped` vectors and re-parsing each `ObjectHash`. Returns
    /// `Ok(None)` when no row exists.
    /// The row of an EXPLICITLY resolved scope (§C.4.2) — the `ORIG_HEAD`
    /// projection of §C.5 reads it for the scope its caller resolved.
    pub(crate) async fn load_for_scope(
        scope: &crate::internal::worktree_scope::WorktreeScope,
    ) -> Result<Option<BisectState>, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::load_from_db_in_scope(&db, scope.storage_key()).await
    }

    async fn load_from_db<C: ConnectionTrait>(db: &C) -> Result<Option<BisectState>, String> {
        Self::load_from_db_in_scope(db, &Self::scope_key()).await
    }

    async fn load_from_db_in_scope<C: ConnectionTrait>(
        db: &C,
        scope_key: &str,
    ) -> Result<Option<BisectState>, String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT orig_head, orig_head_name, bad, good, current, skipped, steps, completed, first_parent FROM bisect_state WHERE worktree_id = ? LIMIT 1;",
            [scope_key.into()],
        );

        if let Some(result) = db
            .query_one_raw(stmt)
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

            let orig_head = crate::internal::object_format::parse_repo_oid(&orig_head_str)
                .map_err(|e| format!("invalid orig_head hash: {e}"))?;

            let bad = bad_str.and_then(|s| crate::internal::object_format::parse_repo_oid(&s).ok());

            let good: Vec<ObjectHash> = serde_json::from_str(&good_json)
                .map_err(|e| format!("failed to parse good commits: {e}"))?;

            let current =
                current_str.and_then(|s| crate::internal::object_format::parse_repo_oid(&s).ok());

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

        db.execute_raw(stmt)
            .await
            .map_err(|e| format!("failed to clear bisect state: {e}"))?;

        Ok(())
    }
}

/// The user-facing terms of a bisect session (ADR-MX-11).
///
/// `good`/`bad` (aliases `old`/`new`) are the defaults. Custom terms are
/// supplied at `bisect start --term-old/--term-new` and must survive the whole
/// session so `bisect terms`, `bisect log`, `bisect view` and the custom-term
/// mark subcommands can read them back.
///
/// ER-MX-04: terms are persisted in the repo config store (`config_kv`) under
/// worktree-scoped keys, NOT as new `bisect_state` columns — so no schema
/// migration is required. Keys are cleared by `bisect reset`; a stale key left
/// by an abandoned session is overwritten by the next `bisect start`.
#[derive(Debug, Clone, Eq, PartialEq)]
struct BisectTerms {
    /// Old/good term (default `good`, alias `old`).
    good: String,
    /// New/bad term (default `bad`, alias `new`).
    bad: String,
}

impl BisectTerms {
    fn default_terms() -> Self {
        Self {
            good: "good".to_string(),
            bad: "bad".to_string(),
        }
    }

    /// `config_kv` keys for this worktree's terms. The empty main-worktree
    /// scope key yields the bare `bisect.term.<role>` key; a linked worktree
    /// appends its id so concurrent sessions keep independent terms.
    fn config_key(role: &str) -> String {
        let scope = BisectState::scope_key();
        if scope.is_empty() {
            format!("bisect.term.{role}")
        } else {
            format!("bisect.term.{role}.{scope}")
        }
    }

    /// Load this worktree's terms. `None` means the terms are NOT fixed yet
    /// (a session started without bounds or term options; git's missing
    /// `BISECT_TERMS` file) — the first `bad`/`good`/`new`/`old` mark fixes
    /// them. A partial key set (older binary) falls back per role.
    async fn load() -> Result<Option<Self>, String> {
        let good = ConfigKv::get(&Self::config_key("good"))
            .await
            .map_err(|e| format!("failed to read bisect term-good: {e}"))?;
        let bad = ConfigKv::get(&Self::config_key("bad"))
            .await
            .map_err(|e| format!("failed to read bisect term-bad: {e}"))?;
        Ok(match (good, bad) {
            (None, None) => None,
            (good, bad) => Some(Self {
                good: good
                    .map(|entry| entry.value)
                    .unwrap_or_else(|| "good".to_string()),
                bad: bad
                    .map(|entry| entry.value)
                    .unwrap_or_else(|| "bad".to_string()),
            }),
        })
    }

    async fn save(&self) -> Result<(), String> {
        ConfigKv::set(&Self::config_key("good"), &self.good, false)
            .await
            .map_err(|e| format!("failed to save bisect term-good: {e}"))?;
        ConfigKv::set(&Self::config_key("bad"), &self.bad, false)
            .await
            .map_err(|e| format!("failed to save bisect term-bad: {e}"))
    }

    async fn clear() -> Result<(), String> {
        for role in ["good", "bad"] {
            ConfigKv::unset(&Self::config_key(role))
                .await
                .map_err(|e| format!("failed to clear bisect term-{role}: {e}"))?;
        }
        Ok(())
    }
}

/// Bisect subcommands that can never be reused as custom terms (git
/// `bisect.c::check_term_format`). `bad`/`good`/`new`/`old` are deliberately
/// absent: they are mark subcommands whose meaning is protected separately.
const BISECT_BUILTIN_COMMANDS: &[&str] = &[
    "start",
    "skip",
    "next",
    "reset",
    "visualize",
    "view",
    "replay",
    "log",
    "run",
    "terms",
    "help",
];

/// Which side of the bisect a candidate term is being validated for.
#[derive(Clone, Copy)]
enum TermSide {
    /// The old/good side.
    Good,
    /// The new/bad side.
    Bad,
}

/// Validate one custom term against git's `check_term_format` rules
/// (ADR-MX-11 §4 / M-BTERMS T5). Returns a user-facing error string.
fn validate_term(term: &str, side: TermSide) -> Result<(), String> {
    // git validates `refs/bisect/<term>` with check_refname_format; reuse the
    // shared refname checker (matches git's check-ref-format rules) so terms
    // like `fa st`, `a..b` or control characters are refused.
    if term.is_empty() || !util::is_valid_refname(&format!("refs/bisect/{term}")) {
        return Err(format!("'{term}' is not a valid term"));
    }
    if BISECT_BUILTIN_COMMANDS.contains(&term) {
        return Err(format!("can't use the builtin command '{term}' as a term"));
    }
    // Swapping the meaning wholesale is forbidden (git keeps this
    // untested-on-purpose). The old side must not steal `bad`/`new`, the
    // new side must not steal `good`/`old`.
    match side {
        TermSide::Good => {
            if term == "bad" || term == "new" {
                return Err(format!("can't change the meaning of the term '{term}'"));
            }
        }
        TermSide::Bad => {
            if term == "good" || term == "old" {
                return Err(format!("can't change the meaning of the term '{term}'"));
            }
        }
    }
    Ok(())
}

/// Resolve `--term-old/--term-new` into a [`BisectTerms`], validating both
/// (git rejects identical terms with "please use two different terms").
fn resolve_bisect_terms(term_old: Option<&str>, term_new: Option<&str>) -> CliResult<BisectTerms> {
    let terms = BisectTerms {
        good: term_old.unwrap_or("good").to_string(),
        bad: term_new.unwrap_or("bad").to_string(),
    };
    if terms.good == terms.bad {
        return Err(CliError::command_usage("please use two different terms"));
    }
    validate_term(&terms.good, TermSide::Good).map_err(CliError::command_usage)?;
    validate_term(&terms.bad, TermSide::Bad).map_err(CliError::command_usage)?;
    Ok(terms)
}

/// ADR-MX-05 §2: whether the start gate must refuse, given the two status
/// snapshots. Only TRACKED files count: staged changes of any kind plus
/// unstaged tracked modifications (modified/deleted/renamed). Untracked and
/// ignored files are deliberately excluded — a candidate whose checkout would
/// overwrite them is refused by the per-step restore preflight instead.
fn tracked_changes_block_start(staged: &Changes, unstaged: &Changes) -> bool {
    !staged.is_empty()
        || !unstaged.modified.is_empty()
        || !unstaged.deleted.is_empty()
        || !unstaged.renamed.is_empty()
}

/// `--help` examples shown in `libra bisect --help` output.
pub const BISECT_EXAMPLES: &str = "\
EXAMPLES:
    libra bisect start                         Begin a session; mark bad/good in subsequent steps
    libra bisect start <bad>                   Begin a session with the bad commit pre-marked
    libra bisect start <bad> <good>            Begin a session with both bounds pre-marked
    libra bisect start <bad> --good <good>     Compatibility form of the positional <good>
    libra bisect start <bad> --first-parent    Bisect only the first-parent (mainline) history
    libra bisect start --term-old fast --term-new slow <bad> <good>
                                               Use custom terms 'fast'/'slow'
    libra bisect bad                           Mark the current HEAD as bad
    libra bisect new                           Mark the current HEAD as new (sets new/old terms)
    libra bisect good                          Mark the current HEAD as good
    libra bisect old                           Mark the current HEAD as old (sets new/old terms)
    libra bisect skip                          Skip the current commit and continue
    libra bisect terms                         Show the current old/new terms
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
        #[serde(skip_serializing_if = "Option::is_none")]
        term_bad: Option<String>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        term_bad: Option<String>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        term_good: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        term_bad: Option<String>,
    },
    Terms {
        #[serde(skip_serializing_if = "Option::is_none")]
        good: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bad: Option<String>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        term_good: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        term_bad: Option<String>,
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
            good_opt,
            first_parent,
            term_old,
            term_new,
        } => {
            let mut goods = good;
            if let Some(good) = good_opt {
                goods.push(good);
            }
            // Repeatable term options: last one wins (git `t6030:1289`).
            let term_old = term_old.last().cloned();
            let term_new = term_new.last().cloned();
            run_bisect_start(bad, goods, first_parent, term_old, term_new).await
        }
        Bisect::Bad { rev } => run_bisect_bad(rev).await,
        Bisect::Good { rev } => run_bisect_good(rev).await,
        Bisect::Reset { rev } => run_bisect_reset(rev).await,
        Bisect::Skip { rev } => run_bisect_skip(rev).await,
        Bisect::Log => run_bisect_log().await,
        Bisect::Run { cmd } => run_bisect_run(cmd).await,
        Bisect::View => run_bisect_view().await,
        Bisect::Terms {
            term_good,
            term_bad,
        } => run_bisect_terms(term_good, term_bad).await,
        Bisect::Term(args) => run_bisect_custom_term(args).await,
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
            term_bad,
            ..
        } => {
            println!("Bisect session started");
            render_bisect_progress(
                status,
                current,
                first_bad,
                subject,
                *remaining,
                term_bad.as_deref(),
            );
        }
        BisectOutput::Mark {
            mark,
            commit,
            status,
            current,
            first_bad,
            subject,
            remaining,
            term_bad,
            ..
        } => {
            println!("Marked {} as {mark}", short_hash_str(commit));
            render_bisect_progress(
                status,
                current,
                first_bad,
                subject,
                *remaining,
                term_bad.as_deref(),
            );
        }
        BisectOutput::Skip {
            commit,
            status,
            current,
            remaining,
            ..
        } => {
            println!("Skipped {}", short_hash_str(commit));
            render_bisect_progress(status, current, &None, &None, *remaining, None);
        }
        BisectOutput::Reset {
            restored,
            commit,
            branch,
        } => {
            if *restored {
                if let Some(commit) = commit {
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
            } else if commit.is_none() {
                println!("We are not bisecting.");
            } else if let Some(commit) = commit {
                // The session row was cleared but the original HEAD could not
                // be restored; the recovery hint was already emitted on stderr.
                println!(
                    "Bisect session ended; original HEAD {} could not be restored",
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
            term_good,
            term_bad,
            ..
        } => {
            let term_bad = term_bad.as_deref().unwrap_or("bad");
            let term_good = term_good.as_deref().unwrap_or("good");
            println!("Bisect log:");
            println!(
                "  {term_bad}: {}",
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
            println!("  {term_good}: {good}");
            println!(
                "  Current: {}",
                current.as_deref().map(short_hash_str).unwrap_or("not set")
            );
            println!("  Skipped: {} commits", skipped.len());
            println!("  Steps remaining: {steps:?}");
        }
        BisectOutput::Terms { good, bad } => match (good, bad) {
            (Some(good), Some(bad)) => {
                println!("Your current terms are '{good}' for the old state");
                println!("and '{bad}' for the new state.");
            }
            (Some(good), None) => println!("{good}"),
            (None, Some(bad)) => println!("{bad}"),
            (None, None) => {}
        },
        BisectOutput::View {
            head,
            good,
            bad,
            remaining,
            skipped,
            term_good,
            term_bad,
            ..
        } => {
            let term_good = term_good.as_deref().unwrap_or("good");
            let term_bad = term_bad.as_deref().unwrap_or("bad");
            let good = good
                .first()
                .map(|hash| short_hash_str(hash).to_string())
                .unwrap_or_else(|| "(unset)".to_string());
            let bad = bad
                .as_deref()
                .map(short_hash_str)
                .unwrap_or("(unset)")
                .to_string();
            println!("Bisecting between {good} ({term_good}) and {bad} ({term_bad})");
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
    term_bad: Option<&str>,
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
                // Custom-term sessions print git's quoted culprit line
                // ("<oid> is the first 'slow' commit", t6030:1188); the
                // default bad/good form keeps the legacy unquoted message
                // pinned by the MX-03 tests.
                match term_bad {
                    Some(term) if term != "bad" => {
                        println!("{} is the first '{term}' commit", short_hash_str(first_bad));
                    }
                    _ => println!("{} is the first bad commit", short_hash_str(first_bad)),
                }
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

/// Extract the human-readable subject line for culprit display.
///
/// Vault-signed commits prepend a `gpgsig`/`gpgsig-sha256` header block to the
/// raw commit message, so `commit.message.lines().next()` would render
/// `gpgsig -----BEGIN PGP SIGNATURE-----` as the culprit subject (issue #481
/// N4). [`Commit::format_message`] already skips that block and returns the
/// first non-empty body line; this wrapper keeps the call sites
/// self-documenting and unit-testable (ADR-MX-03 §4).
fn culprit_subject(commit: &Commit) -> String {
    commit.format_message()
}

/// Build the `converged` [`BisectOutput::Mark`] for a session that has found
/// (or already found) the first bad commit.
///
/// Shared by the normal convergence path and by re-marking after convergence
/// (ADR-MX-03 §3): once the search has converged, `bisect good`/`bisect bad`
/// re-print the culprit instead of surfacing an internal "already found"
/// error.
async fn converged_mark(mark: &str, state: &BisectState) -> CliResult<BisectOutput> {
    let bad = state
        .bad
        .ok_or_else(|| CliError::fatal("No bad commit set"))?;
    let commit = load_object::<Commit>(&bad)
        .map_err(|e| CliError::fatal(format!("Failed to load commit: {e}")))?;
    let subject = culprit_subject(&commit);
    // The culprit line names the session's new/bad term (ADR-MX-11 §3).
    let term_bad = BisectTerms::load()
        .await
        .map_err(CliError::fatal)?
        .map(|terms| terms.bad);
    Ok(BisectOutput::Mark {
        mark: mark.to_string(),
        commit: bad.to_string(),
        status: "converged".to_string(),
        current: hash_to_string_opt(state.current),
        first_bad: Some(bad.to_string()),
        subject: Some(subject),
        remaining: Some(0),
        steps: state.steps,
        term_bad,
    })
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
    use crate::internal::config::parse_git_bool;

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
/// - Refuses to run on bare repos and when TRACKED files carry staged or
///   unstaged changes. Untracked and ignored files no longer block the start
///   (ADR-MX-05 §2): the per-step restore preflight still rejects a candidate
///   whose checkout would overwrite a colliding untracked/ignored path.
/// - Captures the original branch (or detached commit) and stores it inside
///   [`BisectState`] so `reset` can recover it.
/// - Accepts Git's positional form `<bad> [<good>...]`; `--good` is retained
///   as a compatibility alias and merged into the positional goods.
/// - Persists custom terms (ADR-MX-11) so `bisect terms`, `bisect log` and
///   `bisect view` can render them; `reset` clears them.
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
    goods: Vec<String>,
    first_parent: bool,
    term_old: Option<String>,
    term_new: Option<String>,
) -> CliResult<BisectOutput> {
    // Bare repositories have no working tree - bisect requires checkout operations
    if is_bare_repository().await? {
        return Err(CliError::fatal("bisect cannot be run in a bare repository")
            .with_hint("bisect requires a working tree to check out commits for testing"));
    }

    // §C.4.4 bisect/sequencer mutual exclusion: a rebase, cherry-pick, revert,
    // merge or am owning THIS worktree's scope blocks a new bisect, exactly as
    // an active bisect now blocks them. Bisect was outside the symmetric mutex
    // entirely, so a `bisect start` could begin beside an in-progress sequence
    // and then check out candidates underneath it.
    //
    // Checked BEFORE the clean-tree requirement: an in-progress sequence is
    // the more fundamental blocker, and it is usually what made the tree
    // dirty — "working tree contains uncommitted changes" would send the user
    // to commit conflict markers.
    crate::internal::sequencer::ensure_none_for_bisect().await?;

    // ADR-MX-05 §2: the start gate only considers TRACKED files (staged plus
    // tracked worktree modifications). Untracked and ignored files no longer
    // block the start — a candidate that tracks a colliding path is instead
    // refused by the per-step restore preflight before any state is written.
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(|e| CliError::fatal(format!("Failed to check staged changes: {e}")))?;
    let unstaged = changes_to_be_staged()
        .map_err(|e| CliError::fatal(format!("Failed to check unstaged changes: {e}")))?;
    if tracked_changes_block_start(&staged, &unstaged) {
        return Err(CliError::fatal(
            "working tree contains uncommitted changes to tracked files",
        )
        .with_hint("commit or stash your changes before running bisect. Note: each 'bisect good/bad/skip' step resets tracked files in the index and working tree to the candidate commit, so uncommitted tracked changes would be lost."));
    }

    // ADR-MX-11 §4: validate the custom terms BEFORE touching any state so an
    // invalid term is rejected with zero writes (M-BTERMS T5).
    //
    // git fixes the terms at start when term options OR bounds are given
    // (it creates `refs/bisect/<term>` for the bounds immediately); a
    // bound-less `bisect start` leaves them unfixed until the first mark.
    let term_given = term_old.is_some() || term_new.is_some();
    let bounds_given = bad.is_some() || !goods.is_empty();
    let terms = if term_given || bounds_given {
        Some(resolve_bisect_terms(
            term_old.as_deref(),
            term_new.as_deref(),
        )?)
    } else {
        None
    };

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

    let mut good_hashes = Vec::with_capacity(goods.len());
    for good_ref in goods {
        good_hashes.push(resolve_ref(&good_ref).await?);
    }

    let mut state = BisectState {
        orig_head,
        orig_head_name,
        bad: bad_hash,
        good: good_hashes,
        current: None,
        skipped: vec![],
        steps: None,
        completed: false,
        first_parent,
    };

    // Validate the initial checkout before claiming the session, so a known
    // restore refusal cannot leave an orphaned bisect row behind.
    let initial_next = match (bad_hash, state.good.is_empty()) {
        (Some(bad), false) => {
            let next = find_next_bisect_point(&state)
                .await
                .map_err(CliError::fatal)?;
            match &next {
                BisectNext::Next(candidate) => {
                    restore::preflight_worktree_restore_to_commit(candidate).await?;
                }
                BisectNext::Converged => {
                    restore::preflight_worktree_restore_to_commit(&bad).await?;
                }
                BisectNext::AllSkipped => {}
            }
            Some(next)
        }
        _ => None,
    };

    // The first write of a STARTING session is a claim, not an upsert
    // (§C.4.4) — see `BisectState::claim_start`.
    state.claim_start().await.map_err(CliError::fatal)?;

    // ADR-MX-11 §3: persist the terms AFTER the session claim wins, so a
    // refused claim cannot leave stale term keys behind (M-BTERMS T5). A
    // bound-less start without term options leaves them unfixed; the first
    // mark then fixes them (git's `BISECT_TERMS` semantics).
    match terms.as_ref() {
        Some(terms) => terms.save().await.map_err(CliError::fatal)?,
        None => clear_bisect_terms_best_effort().await,
    };
    // The bad/new term for culprit lines in Start outputs (None when the
    // session is bound-less and terms are unfixed).
    let term_bad_out = terms.as_ref().map(|terms| terms.bad.clone());

    if state.bad.is_some() && state.good.is_empty() {
        return Ok(BisectOutput::Start {
            status: "waiting_for_good".to_string(),
            bad: hash_to_string_opt(state.bad),
            good: hashes_to_strings(&state.good),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
            term_bad: term_bad_out.clone(),
        });
    }

    // If good is provided but no bad, wait for bad
    if !state.good.is_empty() && state.bad.is_none() {
        return Ok(BisectOutput::Start {
            status: "waiting_for_bad".to_string(),
            bad: hash_to_string_opt(state.bad),
            good: hashes_to_strings(&state.good),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
            term_bad: term_bad_out.clone(),
        });
    }

    // Reuse the candidate whose restore was validated before claiming state.
    if let Some(next) = initial_next {
        match next {
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
                    term_bad: term_bad_out.clone(),
                });
            }
            BisectNext::Converged => {
                // Only one commit between bad and good - it's the culprit
                let bad_commit = state.bad.ok_or_else(|| CliError::fatal("No bad commit"))?;
                let commit = load_object::<Commit>(&bad_commit)
                    .map_err(|e| CliError::fatal(format!("Failed to load commit: {e}")))?;
                let subject = culprit_subject(&commit);
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
                    term_bad: term_bad_out.clone(),
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
                    term_bad: term_bad_out.clone(),
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
        term_bad: term_bad_out.clone(),
    })
}

/// git `bisect.c::check_and_set_terms` equivalent for mark subcommands.
///
/// - Terms FIXED (persisted by `start` with bounds or `--term-*`, or by an
///   earlier mark): the subcommand must be exactly the term for its side —
///   a `slow`/`fast` session refuses `bad`/`good`/`new`/`old`, a `bad`/`good`
///   session refuses `new`/`old`/custom marks, and a `new`/`old` session
///   refuses `bad`/`good` (git prints `Invalid command: you're currently in
///   a %s/%s bisect` + usage).
/// - Terms UNFIXED (bound-less `bisect start`): the first mark fixes them to
///   `bad`/`good` (or `new`/`old` when the mark was `new`/`old`).
async fn ensure_mark_terms(cmd_name: &str, side: TermSide) -> CliResult<()> {
    match BisectTerms::load().await.map_err(CliError::fatal)? {
        Some(terms) => {
            let expected = match side {
                TermSide::Good => &terms.good,
                TermSide::Bad => &terms.bad,
            };
            if expected != cmd_name {
                return Err(invalid_bisect_term_command(&terms, cmd_name));
            }
        }
        None => {
            // Custom terms can never be unfixed: `start --term-*` persists
            // them, so only bad/good/new/old reach this branch.
            let terms = if cmd_name == "new" || cmd_name == "old" {
                BisectTerms {
                    good: "old".to_string(),
                    bad: "new".to_string(),
                }
            } else {
                BisectTerms::default_terms()
            };
            terms.save().await.map_err(CliError::fatal)?;
        }
    }
    Ok(())
}

/// The git `Invalid command: you're currently in a %s/%s bisect` usage error
/// for a mark subcommand that does not match the session's fixed terms.
fn invalid_bisect_term_command(terms: &BisectTerms, cmd_name: &str) -> CliError {
    CliError::command_usage(format!(
        "Invalid command: you're currently in a {}/{} bisect\nunknown command: '{cmd_name}'",
        terms.bad, terms.good,
    ))
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
    // ADR-MX-03 §5: no-session `bad` shares the stable `LBR-BISECT-001`
    // error with `good`/`skip`/`log`/`view`/`run` instead of surfacing an
    // internal "No bisect in progress" load failure.
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }
    // ADR-MX-11 §2: `bad` is only accepted while the session's terms are
    // still bad/good (or unfixed — this fixes them to bad/good).
    ensure_mark_terms("bad", TermSide::Bad).await?;
    run_bisect_bad_mark(rev, "bad").await
}

/// [`run_bisect_bad`] body, parameterised by the subcommand name the user
/// typed (`bad`, `new`, or a custom term) for output and terms enforcement.
async fn run_bisect_bad_mark(rev: Option<String>, cmd_name: &str) -> CliResult<BisectOutput> {
    let mut state = BisectState::load().await.map_err(CliError::fatal)?;

    // ADR-MX-03 §3: after convergence, re-marking re-prints the culprit
    // (exit 0) instead of erroring with an internal "already found" message.
    if state.completed {
        return converged_mark(cmd_name, &state).await;
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
            mark: cmd_name.to_string(),
            commit: bad_hash.to_string(),
            status: "waiting_for_good".to_string(),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
            term_bad: None,
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
                mark: cmd_name.to_string(),
                commit: bad_hash.to_string(),
                status: "testing".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining,
                steps: state.steps,
                term_bad: None,
            })
        }
        BisectNext::Converged => {
            // We found the culprit!
            let bad = state
                .bad
                .ok_or_else(|| CliError::fatal("No bad commit set"))?;
            // Move HEAD to the culprit commit, mark completed but keep state for reset
            checkout_to_commit(bad).await?;
            state.current = Some(bad);
            state.completed = true;
            state.save().await.map_err(CliError::fatal)?;
            converged_mark(cmd_name, &state).await
        }
        BisectNext::AllSkipped => {
            // Bounds valid but all candidates skipped - mark is saved
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: cmd_name.to_string(),
                commit: bad_hash.to_string(),
                status: "all_skipped".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining: Some(0),
                steps: state.steps,
                term_bad: None,
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
    // ADR-MX-03 §5: see `run_bisect_bad` — unify the no-session error.
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }
    // ADR-MX-11 §2: `good` is only accepted while the session's terms are
    // still bad/good (or unfixed — this fixes them to bad/good).
    ensure_mark_terms("good", TermSide::Good).await?;
    run_bisect_good_mark(rev, "good").await
}

/// [`run_bisect_good`] body, parameterised by the subcommand name the user
/// typed (`good`, `old`, or a custom term) for output and terms enforcement.
async fn run_bisect_good_mark(rev: Option<String>, cmd_name: &str) -> CliResult<BisectOutput> {
    let mut state = BisectState::load().await.map_err(CliError::fatal)?;

    // ADR-MX-03 §3: after convergence, re-marking re-prints the culprit.
    if state.completed {
        return converged_mark(cmd_name, &state).await;
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
            mark: cmd_name.to_string(),
            commit: good_hash.to_string(),
            status: "waiting_for_bad".to_string(),
            current: hash_to_string_opt(state.current),
            first_bad: None,
            subject: None,
            remaining: None,
            steps: state.steps,
            term_bad: None,
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
                mark: cmd_name.to_string(),
                commit: good_hash.to_string(),
                status: "testing".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining,
                steps: state.steps,
                term_bad: None,
            })
        }
        BisectNext::Converged => {
            // We found the culprit!
            let bad = state
                .bad
                .ok_or_else(|| CliError::fatal("No bad commit set"))?;
            // Move HEAD to the culprit commit, mark completed but keep state for reset
            checkout_to_commit(bad).await?;
            state.current = Some(bad);
            state.completed = true;
            state.save().await.map_err(CliError::fatal)?;
            converged_mark(cmd_name, &state).await
        }
        BisectNext::AllSkipped => {
            // Bounds valid but all candidates skipped - mark is saved
            state.save().await.map_err(CliError::fatal)?;
            Ok(BisectOutput::Mark {
                mark: cmd_name.to_string(),
                commit: good_hash.to_string(),
                status: "all_skipped".to_string(),
                current: hash_to_string_opt(state.current),
                first_bad: None,
                subject: None,
                remaining: Some(0),
                steps: state.steps,
                term_bad: None,
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
/// - With no state row at all, prints "We are not bisecting." and returns
///   `Ok(())` — `reset` is the supported escape hatch for stale state.
/// - If restoring the original HEAD fails, the session row is STILL cleared
///   (ADR-MX-03 §2) and a recovery hint naming the original branch/commit is
///   emitted so the user can retry the checkout manually.
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
            // §C.4.4: fail CLOSED on the DANGEROUS action. If the probe
            // cannot answer, do not re-attach — ending detached is
            // recoverable with one `switch`; two worktrees on one branch is
            // not.
            match Head::branch_checked_out_elsewhere_result(&branch_name).await {
                Ok(Some(other)) => {
                    crate::utils::error::emit_warning(format!(
                        "not re-attaching branch '{branch_name}': it is now checked out at \
                         worktree '{other}'; bisect ends detached at {target_hash} \
                         (use 'libra switch' to pick a branch)"
                    ));
                    None
                }
                Ok(None) => Some(branch_name),
                Err(error) => {
                    crate::utils::error::emit_warning(format!(
                        "not re-attaching branch '{branch_name}': cannot determine whether it \
                         is checked out in another worktree ({error}); bisect ends detached at \
                         {target_hash} (use 'libra switch' to pick a branch)"
                    ));
                    None
                }
            }
        }
        None => None,
    };

    // Restore original HEAD - use branch if available to avoid detached state.
    // ADR-MX-03 §2: a restore failure must NOT leave the session wedged —
    // clear the row anyway, and preserve the original branch/commit record in
    // a recovery hint so the user can retry the checkout manually.
    let restore_result = if let Some(branch_name) = target_branch.clone() {
        restore_to_branch(branch_name, target_hash).await
    } else {
        checkout_to_commit(target_hash).await
    };

    match restore_result {
        Ok(()) => {
            // Clean up bisect state
            BisectState::cleanup().await.map_err(CliError::fatal)?;
            clear_bisect_terms_best_effort().await;
            Ok(BisectOutput::Reset {
                restored: true,
                commit: Some(target_hash.to_string()),
                branch: target_branch,
            })
        }
        Err(error) => {
            let target = target_branch
                .as_deref()
                .map(|name| format!("branch '{name}' ({target_hash})"))
                .unwrap_or_else(|| target_hash.to_string());
            crate::utils::error::emit_warning(format!(
                "failed to restore original HEAD {target}: {error}; clearing the bisect \
                 session so it is not wedged — restore manually with 'libra switch' or \
                 'libra checkout'"
            ));
            BisectState::cleanup().await.map_err(CliError::fatal)?;
            clear_bisect_terms_best_effort().await;
            Ok(BisectOutput::Reset {
                restored: false,
                commit: Some(target_hash.to_string()),
                branch: target_branch,
            })
        }
    }
}

/// Clear the persisted bisect terms after a session ends. Best-effort: terms
/// are never read without a session (`log`/`view`/`terms` all check
/// `BisectState::has_state` first), so a stale key is harmless and the next
/// `bisect start` overwrites it.
async fn clear_bisect_terms_best_effort() {
    if let Err(error) = BisectTerms::clear().await {
        crate::utils::error::emit_warning(format!("failed to clear bisect terms: {error}"));
    }
}

fn map_bisect_branch_store_error(branch_name: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': failed to query branch storage: {detail}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("repair branch storage or reset to an explicit revision with 'libra bisect reset <rev>'."),
        BranchStoreError::AlreadyExists(name) => CliError::fatal(format!(
            "branch '{name}' already exists"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget),
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
        // §C.13: a checkout collision is a CONFLICT, not corruption or an
        // I/O fault — the repository is intact and the user has a next step.
        BranchStoreError::CheckedOutElsewhere { .. } => CliError::fatal(format!(
            "failed to restore original branch '{branch_name}': {error}"
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint(
            "the other worktree must switch away first; `libra worktree list` shows which one \
             holds it",
        ),
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
    restore::preflight_worktree_restore_to_commit(&commit_hash).await?;
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;

    let txn = crate::internal::db::begin_write_transaction(&db)
        .await
        .map_err(|e| CliError::fatal(format!("Failed to begin transaction: {e}")))?;

    // Update HEAD to point to the branch
    let new_head = Head::Branch(branch_name.clone());
    Head::update_result_with_conn(&txn, new_head.clone(), None)
        .await
        .map_err(|error| map_bisect_branch_store_error(&branch_name, error))?;

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
    // ADR-MX-03 §5: unify the no-session error with the rest of the family.
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }

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
    // ADR-MX-03 §5: `log` without a session reports `LBR-BISECT-001` like
    // the other session-requiring subcommands, instead of a raw load failure.
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }

    let state = BisectState::load().await.map_err(CliError::fatal)?;
    let terms = BisectTerms::load()
        .await
        .map_err(CliError::fatal)?
        .unwrap_or_else(BisectTerms::default_terms);

    Ok(BisectOutput::Log {
        bad: hash_to_string_opt(state.bad),
        good: hashes_to_strings(&state.good),
        current: hash_to_string_opt(state.current),
        skipped: hashes_to_strings(&state.skipped),
        steps: state.steps,
        completed: state.completed,
        term_good: Some(terms.good),
        term_bad: Some(terms.bad),
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
    let terms = BisectTerms::load()
        .await
        .map_err(CliError::fatal)?
        .unwrap_or_else(BisectTerms::default_terms);

    Ok(BisectOutput::View {
        head,
        good: hashes_to_strings(&state.good),
        bad: hash_to_string_opt(state.bad),
        current: hash_to_string_opt(state.current),
        remaining,
        skipped: hashes_to_strings(&state.skipped),
        completed: state.completed,
        term_good: Some(terms.good),
        term_bad: Some(terms.bad),
    })
}

/// Handle `bisect terms [--term-good|--term-bad]` — report the session's terms.
///
/// Behavior (ADR-MX-11 §2):
/// - No option: prints git's two-line "Your current terms are ..." text.
/// - `--term-good`/`--term-old`: prints only the old/good term.
/// - `--term-bad`/`--term-new`: prints only the new/bad term.
/// - No session: unifies with the rest of the family (`LBR-BISECT-001`, 128),
///   matching M-BTERMS T3's "无会话按 S5".
/// - Session without fixed terms (bound-less start, no mark yet): mirrors
///   git's `error: no terms defined`.
async fn run_bisect_terms(term_good: bool, term_bad: bool) -> CliResult<BisectOutput> {
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(BisectError::NotActive.into());
    }

    let terms = match BisectTerms::load().await.map_err(CliError::fatal)? {
        Some(terms) => terms,
        // Mid-session state, not an internal failure: the session exists but
        // no mark has fixed the terms yet (git: `error: no terms defined`).
        None => {
            return Err(CliError::fatal("no terms defined")
                .with_stable_code(StableErrorCode::RepoStateInvalid));
        }
    };

    if term_good {
        return Ok(BisectOutput::Terms {
            good: Some(terms.good),
            bad: None,
        });
    }
    if term_bad {
        return Ok(BisectOutput::Terms {
            good: None,
            bad: Some(terms.bad),
        });
    }
    Ok(BisectOutput::Terms {
        good: Some(terms.good),
        bad: Some(terms.bad),
    })
}

/// Handle a custom bisect term mark (ADR-MX-11 §2).
///
/// `new`/`old` arrive here too (they are not static aliases — see git's
/// `check_and_set_terms`): with FIXED terms they are accepted only when they
/// match the session's terms, and with UNFIXED terms (bound-less start) the
/// first `new`/`old` mark fixes the session to new/old terms. A name that
/// matches neither is git's `unknown command` usage error (129).
async fn run_bisect_custom_term(args: Vec<String>) -> CliResult<BisectOutput> {
    let Some(name) = args.first() else {
        return Err(CliError::command_usage("bisect requires a subcommand"));
    };

    // Custom terms only exist inside a session: without one we cannot tell a
    // term apart from an unknown command, and git reports the latter (129).
    if !BisectState::has_state().await.map_err(CliError::fatal)? {
        return Err(CliError::command_usage(format!(
            "unknown command: '{name}'"
        )));
    }

    if args.len() > 2 {
        return Err(CliError::command_usage(format!(
            "too many arguments for 'bisect {name}'"
        )));
    }

    let rev = args.get(1).cloned();
    match BisectTerms::load().await.map_err(CliError::fatal)? {
        Some(terms) => {
            if name == &terms.bad {
                return run_bisect_bad_mark(rev, name).await;
            }
            if name == &terms.good {
                return run_bisect_good_mark(rev, name).await;
            }
            Err(invalid_bisect_term_command(&terms, name))
        }
        None => {
            // First mark in a bound-less session fixes the terms.
            if name == "new" {
                ensure_mark_terms("new", TermSide::Bad).await?;
                return run_bisect_bad_mark(rev, name).await;
            }
            if name == "old" {
                ensure_mark_terms("old", TermSide::Good).await?;
                return run_bisect_good_mark(rev, name).await;
            }
            Err(CliError::command_usage(format!(
                "unknown command: '{name}'"
            )))
        }
    }
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

    // Marks must use the session's term names so a custom-term session
    // (`fast`/`slow`) keeps converging instead of hitting the `bad`/`good`
    // term-mismatch refusal (ADR-MX-11 §2).
    let terms = BisectTerms::load()
        .await
        .map_err(CliError::fatal)?
        .unwrap_or_else(BisectTerms::default_terms);

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
                run_bisect_good_mark(None, &terms.good).await?;
            }
            Some(125) => {
                steps += 1;
                session_skipped.push(head_short.clone());
                run_bisect_skip(None).await?;
            }
            Some(code) if (1..=127).contains(&code) => {
                steps += 1;
                run_bisect_bad_mark(None, &terms.bad).await?;
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
/// - Known directory-transition refusals are checked before publishing HEAD.
/// - The HEAD update happens inside a SQLite transaction. The worktree
///   restore runs after commit, so a later write failure can still leave the
///   worktree out of sync until `bisect reset`.
async fn checkout_to_commit(commit_hash: ObjectHash) -> CliResult<()> {
    restore::preflight_worktree_restore_to_commit(&commit_hash).await?;
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;

    let txn = crate::internal::db::begin_write_transaction(&db)
        .await
        .map_err(|e| CliError::fatal(format!("Failed to begin transaction: {e}")))?;

    let new_head = Head::Detached(commit_hash);
    Head::update_result_with_conn(&txn, new_head, None)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to move HEAD to {commit_hash}: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;

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

/// `ORIG_HEAD` for an explicitly resolved scope — the §C.5 projection. `None`
/// when that worktree has no bisect session; an unreadable row is an ERROR.
pub(crate) async fn orig_head_for_scope(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> Result<Option<String>, String> {
    Ok(BisectState::load_for_scope(scope)
        .await?
        .map(|state| state.orig_head.to_string()))
}

#[cfg(test)]
mod tests {
    use git_internal::{
        hash::HashKind,
        internal::object::signature::{Signature, SignatureType},
    };

    use super::*;

    fn signed_commit(message: &str) -> Commit {
        let sig = Signature {
            signature_type: SignatureType::Author,
            name: "Tester".to_string(),
            email: "t@t.test".to_string(),
            timestamp: 0,
            timezone: "+0000".to_string(),
        };
        Commit {
            id: ObjectHash::zero_for_kind(HashKind::Sha1),
            tree_id: ObjectHash::zero_for_kind(HashKind::Sha1),
            parent_commit_ids: vec![],
            author: sig.clone(),
            committer: sig,
            message: message.to_string(),
        }
    }

    /// Vault-signed commits prepend a `gpgsig` header block to the raw commit
    /// message; `culprit_subject` must return the first body line after the
    /// signature, never the `gpgsig -----BEGIN PGP SIGNATURE-----` header
    /// (issue #481 N4 / ADR-MX-03 §4).
    #[test]
    fn culprit_subject_skips_gpgsig_header() {
        let commit = signed_commit(
            "gpgsig -----BEGIN PGP SIGNATURE-----\n iQFixture\n -----END PGP SIGNATURE-----\n\nintroduce the bug\n\nbody\n",
        );
        assert_eq!(culprit_subject(&commit), "introduce the bug");
    }

    /// A plain (unsigned) commit message keeps its first non-empty line as the
    /// subject.
    #[test]
    fn culprit_subject_returns_first_body_line_without_signature() {
        let commit = signed_commit("fix the thing\n\nlonger body\n");
        assert_eq!(culprit_subject(&commit), "fix the thing");
    }

    fn changes(
        new: &[&str],
        modified: &[&str],
        deleted: &[&str],
        renamed: &[(&str, &str)],
    ) -> Changes {
        Changes {
            new: new.iter().map(std::path::PathBuf::from).collect(),
            modified: modified.iter().map(std::path::PathBuf::from).collect(),
            deleted: deleted.iter().map(std::path::PathBuf::from).collect(),
            renamed: renamed
                .iter()
                .map(|(a, b)| (std::path::PathBuf::from(a), std::path::PathBuf::from(b)))
                .collect(),
        }
    }

    /// ADR-MX-05 §2 (M-BSTART B2/B4): the start gate only refuses TRACKED
    /// changes. Untracked (`new`) files — including `init`'s `.libraignore` —
    /// and ignored files never block `bisect start`.
    #[test]
    fn start_gate_counts_only_tracked_changes() {
        let clean = changes(&[], &[], &[], &[]);
        let untracked_only = changes(&[".libraignore", "notes.txt"], &[], &[], &[]);
        let unstaged_tracked = changes(&["notes.txt"], &["tracked.txt"], &[], &[]);
        let deleted_tracked = changes(&[], &[], &["tracked.txt"], &[]);
        let renamed_tracked = changes(&[], &[], &[], &[("a.txt", "b.txt")]);
        let staged = changes(&["added.txt"], &[], &[], &[]);

        assert!(!tracked_changes_block_start(&clean, &clean));
        assert!(!tracked_changes_block_start(&clean, &untracked_only));
        assert!(tracked_changes_block_start(&clean, &unstaged_tracked));
        assert!(tracked_changes_block_start(&clean, &deleted_tracked));
        assert!(tracked_changes_block_start(&clean, &renamed_tracked));
        assert!(tracked_changes_block_start(&staged, &clean));
    }

    /// ADR-MX-11 §4 (M-BTERMS T5): term validation mirrors git's
    /// `check_term_format` — builtin subcommand names, swapped meanings and
    /// invalid refnames are all refused.
    #[test]
    fn validate_term_matches_git_check_term_format() {
        // Builtin commands can never be terms.
        for builtin in BISECT_BUILTIN_COMMANDS {
            assert!(
                validate_term(builtin, TermSide::Good).is_err(),
                "builtin {builtin} must be refused"
            );
        }
        // Swapping meanings is forbidden (git keeps it untested-on-purpose).
        assert!(validate_term("bad", TermSide::Good).is_err());
        assert!(validate_term("new", TermSide::Good).is_err());
        assert!(validate_term("good", TermSide::Bad).is_err());
        assert!(validate_term("old", TermSide::Bad).is_err());
        // Same-meaning renames are allowed (git accepts `--term-old=old`).
        assert!(validate_term("good", TermSide::Good).is_ok());
        assert!(validate_term("old", TermSide::Good).is_ok());
        assert!(validate_term("bad", TermSide::Bad).is_ok());
        assert!(validate_term("new", TermSide::Bad).is_ok());
        // Refname-format rejection (git validates `refs/bisect/<term>`).
        assert!(validate_term("fa st", TermSide::Bad).is_err());
        assert!(validate_term("invalid..term", TermSide::Bad).is_err());
        assert!(validate_term("", TermSide::Bad).is_err());
        // Free-form terms are fine.
        assert!(validate_term("fast", TermSide::Good).is_ok());
        assert!(validate_term("slow", TermSide::Bad).is_ok());
    }

    /// M-BTERMS T5: identical terms are refused before any state write.
    #[test]
    fn resolve_bisect_terms_rejects_identical_terms() {
        let error = resolve_bisect_terms(Some("slow"), Some("slow")).unwrap_err();
        assert!(error.to_string().contains("two different terms"));
        // Defaults stay bad/good when no term is given.
        let terms = resolve_bisect_terms(None, None).unwrap();
        assert_eq!(terms, BisectTerms::default_terms());
        // Partial custom terms fill the other side with the default.
        let terms = resolve_bisect_terms(Some("fast"), None).unwrap();
        assert_eq!(terms.good, "fast");
        assert_eq!(terms.bad, "bad");
    }

    /// ADR-MX-03 §1 (step atomicity): when a `good` step's restore preflight
    /// is refused — the candidate tracks a path that currently exists as a
    /// non-empty untracked directory — the `good` mark must NOT be persisted:
    /// `current` does not advance and the saved state is byte-for-byte the
    /// pre-failure state.
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn failed_good_step_does_not_modify_bisect_state() {
        use crate::command::{add, commit};

        let temp_path = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = crate::utils::test::ChangeDirGuard::new(temp_path.path());

        // 4 commits (newest first after reverse): C3 removes `blocked` (HEAD),
        // C2/C1 track `blocked` as a file, C0 (good) does not track it.
        let mut hashes = Vec::new();
        for i in 0..4 {
            crate::utils::test::ensure_file("file.txt", Some(&format!("content_{i}\n")));
            if i == 1 || i == 2 {
                crate::utils::test::ensure_file("blocked", Some(&format!("blocked_{i}\n")));
            } else {
                let p = std::path::Path::new("blocked");
                if p.exists() {
                    std::fs::remove_file(p).unwrap();
                }
            }
            add::execute(add::AddArgs {
                intent_to_add: false,
                sparse: false,
                pathspec: vec![],
                all: true,
                update: false,
                refresh: false,
                force: false,
                verbose: false,
                dry_run: false,
                ignore_errors: false,
                pathspec_from_file: None,
                pathspec_file_nul: false,
                chmod: None,
                renormalize: false,
                ignore_missing: false,
                resolved: false,
                patch: false,
                auto_advance: false,
                no_auto_advance: false,
            })
            .await;
            commit::execute(commit::CommitArgs {
                message: Some(format!("Commit {i}")),
                file: None,
                allow_empty: false,
                conventional: false,
                no_edit: false,
                amend: false,
                signoff: false,
                disable_pre: true,
                all: false,
                no_verify: false,
                author: None,
                ..Default::default()
            })
            .await;
            hashes.push(Head::current_commit().await.unwrap().to_string());
        }
        hashes.reverse(); // hashes[0] = newest (C3, HEAD); hashes[3] = oldest (C0)

        // Start with no bounds: HEAD stays at C3, which does not track `blocked`.
        run_bisect_start(None, vec![], false, None, None)
            .await
            .unwrap();

        // Introduce the conflict AFTER start (start refuses a dirty tree): a
        // non-empty untracked directory at a path the midpoint tracks as a file.
        std::fs::create_dir_all("blocked/sub").unwrap();
        std::fs::write("blocked/sub/keep.txt", "x").unwrap();

        // Mark bad = HEAD (C3); no checkout, just records the bound.
        run_bisect_bad(None).await.unwrap();

        let before = BisectState::load().await.unwrap();
        assert!(before.good.is_empty());
        assert!(before.current.is_none());

        // Mark good = C0. The midpoint is C2, which tracks `blocked` as a file;
        // the untracked `blocked/` directory makes the restore preflight refuse.
        let result = run_bisect_good(Some(hashes[3].clone())).await;
        assert!(
            result.is_err(),
            "good step must fail on the blocked restore"
        );

        let after = BisectState::load().await.unwrap();
        assert!(
            after.good.is_empty(),
            "a failed step must not persist the good mark"
        );
        assert_eq!(after.bad.as_ref().unwrap().to_string(), hashes[0]);
        assert!(
            after.current.is_none(),
            "current must not advance on a failed step"
        );

        // The session is not wedged: reset recovers the original HEAD.
        run_bisect_reset(None).await.unwrap();
        assert_eq!(Head::current_commit().await.unwrap().to_string(), hashes[0]);
        assert!(!BisectState::has_state().await.unwrap());
    }
}
