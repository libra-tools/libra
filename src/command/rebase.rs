//! Rebase implementation that parses onto/branch arguments, replays commits onto a new base, handles conflicts, and updates branch refs.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Context;
use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        blob::Blob,
        commit::Commit,
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, Statement,
    TransactionTrait, Value,
};
use serde::{Deserialize, Serialize};

use crate::{
    cli_error,
    command::{load_object, save_object, status, switch},
    common_utils::{format_commit_msg, parse_commit_msg},
    internal::{
        branch::Branch,
        db::get_db_conn_instance,
        head::Head,
        model::{reference as ref_model, reflog as reflog_model},
        reflog,
        reflog::{ReflogAction, ReflogContext, ReflogError, with_reflog},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        ignore::IgnorePolicy,
        object_ext::{BlobExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path, util, worktree,
    },
};

/// Rebase state stored in the repo database
#[derive(Debug, Clone)]
pub struct RebaseState {
    /// Original branch name being rebased
    pub head_name: String,
    /// Commit hash being rebased onto
    pub onto: ObjectHash,
    /// Original HEAD commit before rebase started
    pub orig_head: ObjectHash,
    /// Remaining commits to replay (in order)
    pub todo: VecDeque<ObjectHash>,
    /// Replay action for each remaining commit.
    pub todo_actions: VecDeque<RebaseTodoAction>,
    /// Commits already replayed
    pub done: Vec<ObjectHash>,
    /// Current commit being applied (stopped due to conflict)
    pub stopped_sha: Option<ObjectHash>,
    /// Current new base (HEAD of rebased commits so far)
    pub current_head: ObjectHash,
    /// Whether fixup!/squash! commits should be folded during this rebase.
    pub autosquash: bool,
    /// How to handle commits that *become* empty after replay (Git's `--empty`).
    /// Must survive a conflict + `--continue`, so a later become-empty commit in
    /// the sequence is dropped/kept the same way the start invocation requested.
    pub empty_mode: RebaseEmptyMode,
}

/// Durable options whose lifetime spans a non-interactive rebase sequence.
///
/// The primary todo/current-head state remains in SQLite. These additive
/// controls live in one atomic sidecar so older databases do not need a schema
/// migration and a crash cannot leave half-written exec/update-ref/autostash
/// metadata. The sidecar is removed only after final ref updates and any held
/// autostash have been resolved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RebaseAuxState {
    #[serde(default)]
    exec_commands: Vec<String>,
    /// Index of the command that must be retried by `rebase --continue` after
    /// an `--exec` failure. `None` means no command is pending.
    #[serde(default)]
    pending_exec: Option<usize>,
    #[serde(default)]
    update_refs: bool,
    /// Branches selected at rebase start. Checked-out branches are excluded.
    #[serde(default)]
    refs_to_update: Vec<RebaseRefUpdate>,
    /// Original commit -> rewritten commit, populated after every replayed or
    /// dropped commit so update-refs survives conflicts and process restarts.
    #[serde(default)]
    rewrites: BTreeMap<String, String>,
    /// Original start-empty commit -> its original parent (or the new base).
    /// Used to resolve branches pointing at commits removed by
    /// `--no-keep-empty` once their nearest retained ancestor is rewritten.
    #[serde(default)]
    rewrite_aliases: BTreeMap<String, String>,
    /// Held stash commit, deliberately outside `refs/stash` until the rebase
    /// completes or aborts.
    #[serde(default)]
    autostash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RebaseRefUpdate {
    branch: String,
    old_oid: String,
}

impl RebaseAuxState {
    fn path() -> PathBuf {
        util::storage_path().join("rebase-aux.json")
    }

    fn load_optional() -> Result<Option<Self>, RebaseError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|error| RebaseError::AuxStateLoad {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| RebaseError::AuxStateLoad {
                path: path.display().to_string(),
                detail: error.to_string(),
            })
    }

    fn save(&self) -> Result<(), RebaseError> {
        let path = Self::path();
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| RebaseError::AuxStateSave {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        crate::utils::atomic_write::write_atomic(&path, &bytes, true).map_err(|error| {
            RebaseError::AuxStateSave {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })
    }

    fn cleanup() -> Result<(), RebaseError> {
        let path = Self::path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(RebaseError::AuxStateSave {
                path: path.display().to_string(),
                detail: error.to_string(),
            }),
        }
    }
}

/// Return the held autostash root for repository maintenance. Held objects are
/// intentionally absent from `refs/stash`; GC must trace this sidecar while a
/// rebase is stopped or it can delete the user's only copy of dirty changes.
pub(crate) fn held_autostash_oid() -> CliResult<Option<ObjectHash>> {
    RebaseAuxState::load_optional()
        .map_err(|error| {
            CliError::fatal(format!("failed to load rebase autostash GC root: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?
        .and_then(|aux| aux.autostash)
        .map(|oid| {
            ObjectHash::from_str(&oid).map_err(|error| {
                CliError::fatal(format!(
                    "rebase-aux.json contains invalid autostash object '{oid}': {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })
        })
        .transpose()
}

impl RebaseState {
    /// Get the path to the legacy rebase-merge directory
    fn legacy_rebase_dir() -> PathBuf {
        util::storage_path().join("rebase-merge")
    }

    /// Check if a rebase is in progress
    pub async fn is_in_progress() -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        Self::ensure_rebase_state_table_exists(&db).await?;
        if Self::has_state_in_db(&db).await? {
            return Ok(true);
        }

        if Self::legacy_rebase_dir().exists() {
            return Self::migrate_legacy_state(&db)
                .await
                .map(|state| state.is_some());
        }
        Ok(false)
    }

    /// Save rebase state to the database
    pub async fn save(&self) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        Self::ensure_rebase_state_table_exists(&db).await?;
        Self::save_with_conn(&db, self).await
    }

    /// Load rebase state from the database (migrates legacy files if present)
    pub async fn load() -> Result<Self, String> {
        let db = get_db_conn_instance().await;
        Self::ensure_rebase_state_table_exists(&db).await?;
        if let Some(state) = Self::load_from_db(&db).await? {
            return Ok(state);
        }

        if let Some(state) = Self::migrate_legacy_state(&db).await? {
            return Ok(state);
        }

        Err("No rebase in progress".to_string())
    }

    /// Remove the rebase state from the database (and any legacy state on disk)
    pub async fn cleanup() -> Result<(), String> {
        let db = get_db_conn_instance().await;
        Self::ensure_rebase_state_table_exists(&db).await?;
        Self::clear_state_in_db(&db).await?;

        let legacy_dir = Self::legacy_rebase_dir();
        if legacy_dir.exists() {
            fs::remove_dir_all(&legacy_dir).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn ensure_rebase_state_table_exists<C: ConnectionTrait>(db: &C) -> Result<(), String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type='table' AND name=?;
            "#,
            ["rebase_state".into()],
        );

        if let Some(result) = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to check rebase_state table: {e}"))?
        {
            let count: i64 = result.try_get_by_index(0).unwrap_or(0);
            if count > 0 {
                return Self::ensure_rebase_state_columns(db).await;
            }
        }

        let create_table_stmt = Statement::from_string(
            DbBackend::Sqlite,
            r#"
                CREATE TABLE IF NOT EXISTS `rebase_state` (
                    `id`           INTEGER PRIMARY KEY AUTOINCREMENT,
                    `head_name`    TEXT NOT NULL,
                    `onto`         TEXT NOT NULL,
                    `orig_head`    TEXT NOT NULL,
                    `current_head` TEXT NOT NULL,
                    `todo`         TEXT NOT NULL,
                    `todo_actions` TEXT NOT NULL DEFAULT '',
                    `done`         TEXT NOT NULL,
                    `stopped_sha`  TEXT,
                    `autosquash`   INTEGER NOT NULL DEFAULT 0
                );
            "#
            .to_string(),
        );

        db.execute(create_table_stmt)
            .await
            .map_err(|e| format!("failed to create rebase_state table: {e}"))?;
        Self::ensure_rebase_state_columns(db).await
    }

    async fn ensure_rebase_state_columns<C: ConnectionTrait>(db: &C) -> Result<(), String> {
        let stmt = Statement::from_string(DbBackend::Sqlite, "PRAGMA table_info(rebase_state)");
        let rows = db
            .query_all(stmt)
            .await
            .map_err(|e| format!("failed to inspect rebase_state schema: {e}"))?;
        let mut columns = HashSet::new();
        for row in rows {
            let name: String = row
                .try_get_by_index(1)
                .map_err(|e| format!("failed to inspect rebase_state column: {e}"))?;
            columns.insert(name);
        }
        if !columns.contains("autosquash") {
            let stmt = Statement::from_string(
                DbBackend::Sqlite,
                "ALTER TABLE rebase_state ADD COLUMN autosquash INTEGER NOT NULL DEFAULT 0"
                    .to_string(),
            );
            db.execute(stmt)
                .await
                .map_err(|e| format!("failed to add rebase_state.autosquash: {e}"))?;
        }
        if !columns.contains("todo_actions") {
            let stmt = Statement::from_string(
                DbBackend::Sqlite,
                "ALTER TABLE rebase_state ADD COLUMN todo_actions TEXT NOT NULL DEFAULT ''"
                    .to_string(),
            );
            db.execute(stmt)
                .await
                .map_err(|e| format!("failed to add rebase_state.todo_actions: {e}"))?;
        }
        if !columns.contains("empty_mode") {
            // Default `keep` preserves Libra's existing behavior (replay become-empty
            // commits) for any state row written before this column existed.
            let stmt = Statement::from_string(
                DbBackend::Sqlite,
                "ALTER TABLE rebase_state ADD COLUMN empty_mode TEXT NOT NULL DEFAULT 'keep'"
                    .to_string(),
            );
            db.execute(stmt)
                .await
                .map_err(|e| format!("failed to add rebase_state.empty_mode: {e}"))?;
        }
        Ok(())
    }

    async fn has_state_in_db<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
        let stmt = Statement::from_string(
            DbBackend::Sqlite,
            "SELECT 1 FROM rebase_state LIMIT 1".to_string(),
        );
        let row = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to query rebase_state: {e}"))?;
        Ok(row.is_some())
    }

    async fn load_from_db<C: ConnectionTrait>(db: &C) -> Result<Option<Self>, String> {
        let stmt = Statement::from_string(
            DbBackend::Sqlite,
            r#"
                SELECT head_name, onto, orig_head, current_head, todo, done, stopped_sha, autosquash, todo_actions, empty_mode
                FROM rebase_state
                LIMIT 1
            "#
            .to_string(),
        );
        let row = db
            .query_one(stmt)
            .await
            .map_err(|e| format!("failed to load rebase_state: {e}"))?;
        let Some(row) = row else {
            return Ok(None);
        };

        let head_name: String = row
            .try_get_by_index(0)
            .map_err(|e| format!("invalid head_name: {e}"))?;
        let onto_str: String = row
            .try_get_by_index(1)
            .map_err(|e| format!("invalid onto: {e}"))?;
        let orig_head_str: String = row
            .try_get_by_index(2)
            .map_err(|e| format!("invalid orig_head: {e}"))?;
        let current_head_str: String = row
            .try_get_by_index(3)
            .map_err(|e| format!("invalid current_head: {e}"))?;
        let todo_str: String = row
            .try_get_by_index(4)
            .map_err(|e| format!("invalid todo: {e}"))?;
        let done_str: String = row
            .try_get_by_index(5)
            .map_err(|e| format!("invalid done: {e}"))?;
        let stopped_str: Option<String> = row
            .try_get_by_index(6)
            .map_err(|e| format!("invalid stopped_sha: {e}"))?;
        let autosquash_value: i64 = row
            .try_get_by_index(7)
            .map_err(|e| format!("invalid autosquash: {e}"))?;
        let todo_actions_str: String = row
            .try_get_by_index(8)
            .map_err(|e| format!("invalid todo_actions: {e}"))?;
        let empty_mode_str: String = row
            .try_get_by_index(9)
            .map_err(|e| format!("invalid empty_mode: {e}"))?;
        // Unknown/legacy values fall back to `keep` (Libra's pre-feature behavior).
        let empty_mode =
            parse_rebase_empty_mode(empty_mode_str.trim()).unwrap_or(RebaseEmptyMode::Keep);

        let onto =
            ObjectHash::from_str(onto_str.trim()).map_err(|e| format!("Invalid onto hash: {e}"))?;
        let orig_head = ObjectHash::from_str(orig_head_str.trim())
            .map_err(|e| format!("Invalid orig_head hash: {e}"))?;
        let current_head = ObjectHash::from_str(current_head_str.trim())
            .map_err(|e| format!("Invalid current_head hash: {e}"))?;
        let todo = VecDeque::from(Self::parse_hash_list(&todo_str)?);
        let autosquash = autosquash_value != 0;
        let todo_actions =
            Self::parse_action_list(&todo_actions_str, todo.len(), autosquash, &todo)?;
        let done = Self::parse_hash_list(&done_str)?;
        let stopped_sha = match stopped_str {
            Some(s) if !s.trim().is_empty() => Some(
                ObjectHash::from_str(s.trim())
                    .map_err(|e| format!("Invalid stopped_sha hash: {e}"))?,
            ),
            _ => None,
        };

        Ok(Some(RebaseState {
            head_name,
            onto,
            orig_head,
            todo,
            todo_actions,
            done,
            stopped_sha,
            current_head,
            autosquash,
            empty_mode,
        }))
    }

    async fn save_with_conn<C: ConnectionTrait>(db: &C, state: &RebaseState) -> Result<(), String> {
        let delete_stmt =
            Statement::from_string(DbBackend::Sqlite, "DELETE FROM rebase_state".to_string());
        db.execute(delete_stmt)
            .await
            .map_err(|e| format!("failed to clear existing rebase_state: {e}"))?;

        let todo = Self::format_hash_list(state.todo.iter().cloned());
        let todo_actions = if state.todo_actions.len() == state.todo.len() {
            Self::format_action_list(state.todo_actions.iter().copied())
        } else {
            Self::format_action_list(
                Self::default_todo_actions(&state.todo, state.autosquash)
                    .iter()
                    .copied(),
            )
        };
        let done = Self::format_hash_list(state.done.iter().cloned());
        let stopped_value = match &state.stopped_sha {
            Some(sha) => sha.to_string().into(),
            None => Value::String(None),
        };

        let empty_mode_value = match state.empty_mode {
            RebaseEmptyMode::Drop => "drop",
            RebaseEmptyMode::Keep => "keep",
        };
        let insert_stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
                INSERT INTO rebase_state
                (head_name, onto, orig_head, current_head, todo, todo_actions, done, stopped_sha, autosquash, empty_mode)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
            "#,
            [
                state.head_name.clone().into(),
                state.onto.to_string().into(),
                state.orig_head.to_string().into(),
                state.current_head.to_string().into(),
                todo.into(),
                todo_actions.into(),
                done.into(),
                stopped_value,
                (state.autosquash as i64).into(),
                empty_mode_value.into(),
            ],
        );

        db.execute(insert_stmt)
            .await
            .map_err(|e| format!("failed to save rebase_state: {e}"))?;
        Ok(())
    }

    async fn clear_state_in_db<C: ConnectionTrait>(db: &C) -> Result<(), String> {
        let stmt =
            Statement::from_string(DbBackend::Sqlite, "DELETE FROM rebase_state".to_string());
        db.execute(stmt)
            .await
            .map_err(|e| format!("failed to clear rebase_state: {e}"))?;
        Ok(())
    }

    async fn migrate_legacy_state<C: ConnectionTrait>(db: &C) -> Result<Option<Self>, String> {
        let legacy_dir = Self::legacy_rebase_dir();
        if !legacy_dir.exists() {
            return Ok(None);
        }

        let state = Self::load_from_legacy_dir()?;
        Self::save_with_conn(db, &state).await?;
        if let Err(e) = fs::remove_dir_all(&legacy_dir) {
            emit_warning(format!("failed to remove legacy rebase state: {e}"));
        }
        Ok(Some(state))
    }

    fn load_from_legacy_dir() -> Result<Self, String> {
        let dir = Self::legacy_rebase_dir();
        if !dir.exists() {
            return Err("No rebase in progress".to_string());
        }

        let head_name_raw = fs::read_to_string(dir.join("head-name"))
            .map_err(|e| format!("Failed to read head-name: {}", e))?;
        let head_name = head_name_raw
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(head_name_raw.trim())
            .to_string();

        let onto_str = fs::read_to_string(dir.join("onto"))
            .map_err(|e| format!("Failed to read onto: {}", e))?;
        let onto = ObjectHash::from_str(onto_str.trim())
            .map_err(|e| format!("Invalid onto hash: {}", e))?;

        let orig_head_str = fs::read_to_string(dir.join("orig-head"))
            .map_err(|e| format!("Failed to read orig-head: {}", e))?;
        let orig_head = ObjectHash::from_str(orig_head_str.trim())
            .map_err(|e| format!("Invalid orig-head hash: {}", e))?;

        let current_head_str = fs::read_to_string(dir.join("current-head"))
            .map_err(|e| format!("Failed to read current-head: {}", e))?;
        let current_head = ObjectHash::from_str(current_head_str.trim())
            .map_err(|e| format!("Invalid current-head hash: {}", e))?;

        let todo_content = fs::read_to_string(dir.join("todo")).unwrap_or_default();
        let todo = VecDeque::from(Self::parse_hash_list(&todo_content)?);
        let todo_actions = Self::default_todo_actions(&todo, false);

        let done_content = fs::read_to_string(dir.join("done")).unwrap_or_default();
        let done = Self::parse_hash_list(&done_content)?;

        let stopped_sha = if dir.join("stopped-sha").exists() {
            let stopped_str = fs::read_to_string(dir.join("stopped-sha"))
                .map_err(|e| format!("Failed to read stopped-sha: {}", e))?;
            Some(
                ObjectHash::from_str(stopped_str.trim())
                    .map_err(|e| format!("Invalid stopped-sha hash: {}", e))?,
            )
        } else {
            None
        };

        Ok(RebaseState {
            head_name,
            onto,
            orig_head,
            todo,
            todo_actions,
            done,
            stopped_sha,
            current_head,
            autosquash: false,
            // Legacy on-disk rebase state predates `--empty`; default to keep
            // (Libra's pre-feature behavior).
            empty_mode: RebaseEmptyMode::Keep,
        })
    }

    fn parse_hash_list(content: &str) -> Result<Vec<ObjectHash>, String> {
        let mut commits = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let hash = ObjectHash::from_str(trimmed)
                    .map_err(|e| format!("Invalid commit hash '{}': {}", trimmed, e))?;
                commits.push(hash);
            }
        }
        Ok(commits)
    }

    fn parse_action_list(
        content: &str,
        expected_len: usize,
        autosquash: bool,
        todo: &VecDeque<ObjectHash>,
    ) -> Result<VecDeque<RebaseTodoAction>, String> {
        let tokens: Vec<_> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if tokens.is_empty() {
            return Ok(Self::default_todo_actions(todo, autosquash));
        }
        if tokens.len() != expected_len {
            return Err(format!(
                "invalid todo_actions length: expected {expected_len}, got {}",
                tokens.len()
            ));
        }
        tokens
            .into_iter()
            .map(RebaseTodoAction::from_token)
            .collect()
    }

    fn default_todo_actions(
        todo: &VecDeque<ObjectHash>,
        autosquash: bool,
    ) -> VecDeque<RebaseTodoAction> {
        if !autosquash {
            return todo.iter().map(|_| RebaseTodoAction::Pick).collect();
        }
        todo.iter()
            .map(|commit_id| {
                load_object::<Commit>(commit_id)
                    .map(|commit| RebaseTodoAction::from_message(&commit.message))
                    .unwrap_or(RebaseTodoAction::Pick)
            })
            .collect()
    }

    fn format_hash_list(list: impl IntoIterator<Item = ObjectHash>) -> String {
        let mut out = String::new();
        for (idx, hash) in list.into_iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&hash.to_string());
        }
        out
    }

    fn format_action_list(list: impl IntoIterator<Item = RebaseTodoAction>) -> String {
        let mut out = String::new();
        for (idx, action) in list.into_iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(action.as_str());
        }
        out
    }
}

/// Result of attempting to replay a commit.
///
/// This enum intentionally uses `Conflict` to represent both true merge conflicts and
/// non-conflict failures that should stop the rebase. Callers must examine `message` to
/// distinguish between them and decide whether to prompt for manual resolution or abort.
pub enum ReplayResult {
    /// Commit was successfully replayed; contains the new commit hash.
    Success(ObjectHash),
    /// A user-visible merge conflict was hit while replaying the commit.
    ///
    /// - `paths` lists files left in a conflicted state and waiting for manual resolution.
    /// - `message` is `None` for a clean conflict; it is populated when an IO failure
    ///   happened while materializing the conflict state on disk (e.g. failed to save the
    ///   index with stage 1/2/3 entries, or failed to write a working-tree file).
    Conflict {
        paths: Vec<PathBuf>,
        message: Option<String>,
    },
    /// A non-conflict internal failure occurred (e.g. object load, tree creation,
    /// commit save, index/workdir IO). `kind` classifies the cause so the caller can
    /// surface a precise stable error code; `detail` carries the human-readable cause.
    Internal {
        kind: ReplayErrorKind,
        detail: String,
    },
    /// The commit *became* empty after replay (its merged tree equals the new
    /// parent's tree, though the original commit was not itself empty) and the
    /// effective `--empty` mode is `drop`: skip it without creating a commit. The
    /// index/worktree already match the new parent (the merged tree is identical),
    /// so no mutation is needed. Carries the dropped commit's subject for reporting.
    BecameEmptyDropped { subject: String },
}

/// Policy for a commit that *becomes* empty after replay (Git's `--empty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseEmptyMode {
    /// Skip the become-empty commit (`--empty=drop`, Git's non-interactive default).
    Drop,
    /// Record the now-empty commit (`--empty=keep`; Libra's default when `--empty`
    /// is omitted).
    Keep,
}

/// Parse a `--empty=<mode>` value. Only `drop`/`keep` are supported; Git's
/// `stop`/`ask` (halt for the user to decide) require an interactive-style
/// halt-on-empty resume flow Libra's non-interactive rebase does not have.
/// `None` for an unrecognized or unsupported mode (the caller reports it).
fn parse_rebase_empty_mode(value: &str) -> Option<RebaseEmptyMode> {
    match value {
        "drop" => Some(RebaseEmptyMode::Drop),
        "keep" => Some(RebaseEmptyMode::Keep),
        _ => None,
    }
}

/// Resolve the effective `--empty` mode for a rebase. Omitted → `keep` (Libra's
/// default — an intentional divergence from Git, which drops). `drop`/`keep` are
/// supported; `stop`/`ask` are rejected (no halt-on-empty resume flow); any other
/// value is a usage error. All rejections are `LBR-CLI-002` (exit 129).
fn resolve_rebase_empty_mode(args: &RebaseArgs) -> CliResult<RebaseEmptyMode> {
    let Some(raw) = args.empty.as_deref() else {
        return Ok(RebaseEmptyMode::Keep);
    };
    if let Some(mode) = parse_rebase_empty_mode(raw) {
        return Ok(mode);
    }
    let hint = if matches!(raw, "stop" | "ask") {
        "Libra's non-interactive rebase has no halt-on-empty flow; use --empty=drop or --empty=keep"
    } else {
        "valid values are drop, keep (Git's stop/ask are not supported)"
    };
    Err(
        CliError::command_usage(format!("unrecognized --empty mode '{raw}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(hint),
    )
}

/// Categorizes the cause of a non-conflict failure inside
/// [`replay_commit_with_conflict_detection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayErrorKind {
    IndexLoad,
    CommitLoad,
    MissingParent,
    BaseTreeLoad,
    TheirTreeLoad,
    OurTreeLoad,
    UntrackedOverwrite,
    ConflictMarker,
    TreeCreate,
    CommitSave,
    NewTreeLoad,
    IndexRebuild,
    IndexSave,
    WorkdirReset,
}

impl ReplayErrorKind {
    /// Snake-case identifier surfaced in JSON error details and human messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ReplayErrorKind::IndexLoad => "index_load",
            ReplayErrorKind::CommitLoad => "commit_load",
            ReplayErrorKind::MissingParent => "missing_parent",
            ReplayErrorKind::BaseTreeLoad => "base_tree_load",
            ReplayErrorKind::TheirTreeLoad => "their_tree_load",
            ReplayErrorKind::OurTreeLoad => "our_tree_load",
            ReplayErrorKind::UntrackedOverwrite => "untracked_overwrite",
            ReplayErrorKind::ConflictMarker => "conflict_marker",
            ReplayErrorKind::TreeCreate => "tree_create",
            ReplayErrorKind::CommitSave => "commit_save",
            ReplayErrorKind::NewTreeLoad => "new_tree_load",
            ReplayErrorKind::IndexRebuild => "index_rebuild",
            ReplayErrorKind::IndexSave => "index_save",
            ReplayErrorKind::WorkdirReset => "workdir_reset",
        }
    }

    /// Maps this internal failure cause to its stable error code so distinct
    /// kinds no longer collapse to `ConflictUnresolved`.
    pub fn stable_code(self) -> StableErrorCode {
        match self {
            ReplayErrorKind::IndexLoad => StableErrorCode::IoReadFailed,
            ReplayErrorKind::CommitLoad
            | ReplayErrorKind::MissingParent
            | ReplayErrorKind::BaseTreeLoad
            | ReplayErrorKind::TheirTreeLoad
            | ReplayErrorKind::OurTreeLoad
            | ReplayErrorKind::NewTreeLoad => StableErrorCode::RepoCorrupt,
            ReplayErrorKind::UntrackedOverwrite => StableErrorCode::ConflictOperationBlocked,
            ReplayErrorKind::ConflictMarker
            | ReplayErrorKind::TreeCreate
            | ReplayErrorKind::CommitSave
            | ReplayErrorKind::IndexRebuild
            | ReplayErrorKind::IndexSave
            | ReplayErrorKind::WorkdirReset => StableErrorCode::IoWriteFailed,
        }
    }
}

impl std::fmt::Display for ReplayErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ReplayResult {
    fn conflict(paths: Vec<PathBuf>) -> Self {
        ReplayResult::Conflict {
            paths,
            message: None,
        }
    }

    fn internal(kind: ReplayErrorKind, detail: impl Into<String>) -> Self {
        ReplayResult::Internal {
            kind,
            detail: detail.into(),
        }
    }
}

/// `--help` examples shown in `libra rebase --help` output.
///
/// Rebase exposes a small four-mode state machine: start (positional
/// upstream), `--continue`, `--abort`, `--skip`. The banner pins one
/// example per mode plus a JSON variant so users see all transitions
/// without reading `docs/development/commands/rebase.md`. Cross-cutting `--help`
/// EXAMPLES rollout per `docs/development/commands/_general.md` item B.
pub const REBASE_EXAMPLES: &str = "\
EXAMPLES:
    libra rebase main             Replay current branch on top of main
    libra rebase --autosquash main Fold fixup!/squash! commits while replaying
    libra rebase --reapply-cherry-picks main
    libra rebase --autostash main  Preserve tracked local changes around the rebase
    libra rebase --exec 'cargo test' main  Run a sandboxed command after each replay
    libra rebase --update-refs main  Move other local branches in the rewritten range
    libra rebase --fork-point origin/main  Recover a force-moved upstream fork point
    libra rebase --onto main dev  Replay dev..HEAD onto main, keeping the upstream range
    libra rebase --keep-empty main Keep empty commits while replaying (Libra's default)
    libra rebase --no-keep-empty main  Drop commits that are already empty in the source
    libra rebase --empty=drop main  Drop commits that become empty after replay (already upstream)
    libra rebase --continue       Resume an in-progress rebase after fixing conflicts
    libra rebase --skip           Skip a conflict, or the failed exec command, and continue
    libra rebase --abort          Restore the original branch and clear rebase state
    libra rebase --json main      Structured JSON output for agents";

/// Command-line arguments for the rebase operation
#[derive(Parser, Debug)]
#[command(after_help = REBASE_EXAMPLES)]
pub struct RebaseArgs {
    /// The upstream branch to rebase the current branch onto.
    /// This can be a branch name, commit hash, or other Git reference.
    #[clap(required_unless_present_any = ["continue_rebase", "abort", "skip"])]
    pub upstream: Option<String>,

    /// Replay the <upstream>..HEAD range onto <newbase> instead of onto
    /// <upstream> (the replayed range is still <upstream>..HEAD).
    #[clap(long, value_name = "NEWBASE", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub onto: Option<String>,

    /// Check out <branch> before rebasing; defaults to the current branch.
    #[clap(value_name = "BRANCH", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub branch: Option<String>,

    /// Continue an in-progress rebase after resolving conflicts
    #[clap(long = "continue", conflicts_with_all = ["abort", "skip", "upstream"])]
    pub continue_rebase: bool,

    /// Abort the current rebase and restore the original branch
    #[clap(long, conflicts_with_all = ["continue_rebase", "skip", "upstream"])]
    pub abort: bool,

    /// Skip the current commit and continue with the next
    #[clap(long, conflicts_with_all = ["continue_rebase", "abort", "upstream"])]
    pub skip: bool,

    /// Move fixup!/squash! commits next to their targets and fold them while replaying
    #[clap(long, conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub autosquash: bool,

    /// Explicitly replay clean cherry-pick commits instead of dropping them
    #[clap(long = "reapply-cherry-picks", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub reapply_cherry_picks: bool,

    /// Automatically stash tracked working-tree and index changes before the
    /// rebase, then re-apply them after completion or abort. A conflicting
    /// re-apply is preserved in the normal stash list.
    #[clap(long = "autostash", overrides_with = "no_autostash", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub autostash: bool,

    /// Disable autostash. Last one wins when combined with `--autostash`.
    #[clap(long = "no-autostash", overrides_with = "autostash", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub no_autostash: bool,

    /// Run a shell command after each successfully replayed commit. Commands
    /// execute in a required workspace-write, network-denied Libra sandbox; a
    /// non-zero result stops the rebase and is retried by `--continue`.
    #[clap(long = "exec", value_name = "cmd", action = clap::ArgAction::Append, conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub exec: Vec<String>,

    /// Update other local branches that point into the rewritten commit range.
    /// Branches checked out in any worktree are never moved.
    #[clap(long = "update-refs", overrides_with = "no_update_refs", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub update_refs: bool,

    /// Disable automatic branch updates. Last one wins with `--update-refs`.
    #[clap(long = "no-update-refs", overrides_with = "update_refs", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub no_update_refs: bool,

    /// Use the upstream reflog to find the point where the rebased branch
    /// forked, falling back to the ordinary merge base when no reflog tip is an
    /// ancestor of HEAD.
    #[clap(long = "fork-point", overrides_with = "no_fork_point", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub fork_point: bool,

    /// Use the ordinary merge base even when `--fork-point` was specified
    /// earlier. Last one wins.
    #[clap(long = "no-fork-point", overrides_with = "fork_point", conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub no_fork_point: bool,

    /// Do not update the rerere (reuse recorded resolution) index. Accepted for
    /// Git parity and is a no-op: `libra rerere` exists as a standalone command
    /// but is not yet auto-integrated into rebase, so there is nothing to update
    /// here. (Git's `--rerere-autoupdate` is not exposed.)
    #[clap(long = "no-rerere-autoupdate")]
    pub no_rerere_autoupdate: bool,

    /// Keep commits that begin empty (already empty before replay) rather than
    /// dropping them. Accepted for Git parity and is a no-op: Libra's rebase
    /// already keeps empty commits by default, so this matches existing behavior.
    /// Toggle pair with `--no-keep-empty`; the last one wins. (This controls
    /// commits that *begin* empty; `--empty=<mode>` controls commits that *become*
    /// empty after replay.)
    #[clap(long = "keep-empty", overrides_with = "no_keep_empty")]
    pub keep_empty: bool,

    /// Drop commits that begin empty (their tree equals their parent's — they
    /// introduce no change) instead of replaying them. Toggle pair with
    /// `--keep-empty`; the last one wins. (Only commits that are ALREADY empty are
    /// dropped here; `--empty=<mode>` handles commits that *become* empty after
    /// replay.)
    #[clap(long = "no-keep-empty", overrides_with = "keep_empty")]
    pub no_keep_empty: bool,

    /// How to handle a commit that *becomes* empty after replay (its changes are
    /// already present on the new base): `drop` skips it, `keep` records the empty
    /// commit. Omitted, Libra keeps it (an intentional divergence — Git drops by
    /// default; pass `--empty=drop` for Git's behavior). Git's `stop`/`ask` (halt
    /// for the user to decide) are not supported: Libra's non-interactive rebase
    /// has no halt-on-empty resume flow.
    #[clap(long = "empty", value_name = "mode")]
    pub empty: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RebaseOutput {
    action: String,
    status: String,
    branch: String,
    commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    onto: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    common_ancestor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    applied_commits: Vec<RebaseAppliedCommitOutput>,
    /// Commits skipped under `--empty=drop` (became empty after replay). Additive;
    /// absent when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dropped_commits: Vec<RebaseDroppedCommitOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct RebaseAppliedCommitOutput {
    original_commit: String,
    commit: String,
    subject: String,
}

/// A commit skipped under `--empty=drop` (it became empty after replay).
#[derive(Debug, Clone, Serialize)]
struct RebaseDroppedCommitOutput {
    commit: String,
    subject: String,
}

#[derive(Debug, Default)]
struct RebaseReplaySummary {
    applied_commits: Vec<RebaseAppliedCommitOutput>,
    dropped_commits: Vec<RebaseDroppedCommitOutput>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RebaseError {
    #[error("no rebase in progress")]
    NoRebaseInProgress,
    #[error("failed to check rebase state: {0}")]
    StateCheck(String),
    #[error("failed to load rebase state: {0}")]
    StateLoad(String),
    #[error("failed to load rebase auxiliary state '{path}': {detail}")]
    AuxStateLoad { path: String, detail: String },
    #[error("failed to save rebase auxiliary state '{path}': {detail}")]
    AuxStateSave { path: String, detail: String },
    #[error("not on a branch or in detached HEAD state, cannot rebase")]
    NotOnBranch,
    #[error("current branch '{branch}' has no commits")]
    BranchHasNoCommits { branch: String },
    #[error("failed to resolve upstream '{upstream}': {detail}")]
    UpstreamResolve { upstream: String, detail: String },
    #[error("failed to resolve --onto target '{onto}': {detail}")]
    OntoResolve { onto: String, detail: String },
    #[error("no common ancestor found")]
    NoCommonAncestor,
    #[error("invalid --exec command: {0}")]
    InvalidExec(String),
    #[error(
        "rebase --exec command failed after commit {commit}: {command} (exit {exit_code}){detail}"
    )]
    ExecFailed {
        commit: String,
        command: String,
        exit_code: i32,
        detail: String,
    },
    #[error("failed to prepare rebase update-refs: {0}")]
    UpdateRefs(String),
    #[error("rebase --autostash failed: {0}")]
    Autostash(String),
    #[error("failed to determine working tree status: {0}")]
    WorktreeStatus(String),
    #[error("{detail}, can't {action}")]
    WorktreeDirty { action: String, detail: String },
    #[error("untracked working tree file would be overwritten by rebase: {path}")]
    UntrackedOverwrite { path: String },
    #[error("you must resolve all conflicts before continuing")]
    UnresolvedConflicts,
    #[error("no commit to skip")]
    NoCommitToSkip,
    #[error("rebase stopped while applying {commit}: {subject}")]
    ReplayConflict {
        commit: String,
        subject: String,
        paths: Vec<PathBuf>,
        message: Option<String>,
    },
    #[error("rebase stopped while applying {commit}: {kind} failed ({detail})")]
    ReplayInternal {
        commit: String,
        subject: String,
        kind: ReplayErrorKind,
        detail: String,
    },
    #[error("failed to restore branch '{branch}' during rebase abort: {detail}")]
    BranchRestore { branch: String, detail: String },
    #[error("failed to load commit '{commit}': {detail}")]
    CommitLoad { commit: String, detail: String },
    #[error("failed to load original commit '{commit}': {detail}")]
    OriginalCommitLoad { commit: String, detail: String },
    #[error("failed to load original tree '{tree}': {detail}")]
    OriginalTreeLoad { tree: String, detail: String },
    #[error("failed to load current index: {0}")]
    IndexLoad(String),
    #[error("failed to create tree from index: {0}")]
    TreeCreate(String),
    #[error("failed to save rebased commit: {0}")]
    CommitSave(String),
    #[error("failed to rebuild index: {0}")]
    IndexRebuild(String),
    #[error("failed to save index: {0}")]
    IndexSave(String),
    #[error("failed to reset working directory: {0}")]
    WorkdirReset(String),
    #[error("failed to save rebase state: {0}")]
    StateSave(String),
    #[error("failed to finalize rebase: {0}")]
    Finalize(String),
}

impl From<RebaseError> for CliError {
    fn from(error: RebaseError) -> Self {
        match &error {
            RebaseError::NoRebaseInProgress => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid),
            RebaseError::StateCheck(..)
            | RebaseError::StateLoad(..)
            | RebaseError::AuxStateLoad { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            RebaseError::NotOnBranch | RebaseError::BranchHasNoCommits { .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            }
            RebaseError::UpstreamResolve { .. }
            | RebaseError::OntoResolve { .. }
            | RebaseError::NoCommonAncestor => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget),
            RebaseError::InvalidExec(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("pass a non-empty shell command without NUL bytes"),
            RebaseError::ExecFailed {
                commit,
                command,
                exit_code,
                ..
            } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("fix the command or repository state, then run 'libra rebase --continue'")
                .with_hint("or run 'libra rebase --skip' to keep the applied commit and continue")
                .with_detail("commit", commit.clone())
                .with_detail("command", command.clone())
                .with_detail("exit_code", *exit_code),
            RebaseError::UpdateRefs(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoWriteFailed),
            RebaseError::Autostash(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("inspect 'libra stash list' and re-run the rebase after preserving local changes"),
            RebaseError::WorktreeStatus(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            RebaseError::WorktreeDirty { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("commit or stash your changes before rebasing."),
            RebaseError::UntrackedOverwrite { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("move or remove it before you rebase."),
            RebaseError::UnresolvedConflicts => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::ConflictUnresolved)
                .with_hint("use 'libra add <file>' to mark conflicts as resolved.")
                .with_hint("then run 'libra rebase --continue' again."),
            RebaseError::NoCommitToSkip => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid),
            RebaseError::ReplayConflict {
                commit,
                paths,
                message,
                ..
            } => {
                let mut resolution_hint =
                    "resolve conflicts, stage them, then run 'libra rebase --continue'."
                        .to_string();
                if !paths.is_empty() {
                    let path_list = paths
                        .iter()
                        .map(|path| format!("  {}", path.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    resolution_hint = format!(
                        "conflicted files:\n{path_list}\nresolve conflicts, stage them, then run 'libra rebase --continue'."
                    );
                }
                let mut error = CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::ConflictUnresolved)
                    .with_hint(resolution_hint)
                    .with_hint("or run 'libra rebase --skip' / 'libra rebase --abort'.")
                    .with_detail("commit", commit.clone());
                if !paths.is_empty() {
                    let paths = paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>();
                    error = error.with_detail("paths", serde_json::json!(paths));
                }
                if let Some(message) = message {
                    error = error.with_detail("message", message.clone());
                }
                error
            }
            RebaseError::ReplayInternal {
                commit,
                subject,
                kind,
                detail,
            } => CliError::fatal(error.to_string())
                .with_stable_code(kind.stable_code())
                .with_hint("run 'libra rebase --abort' to return to the original branch.")
                .with_detail("commit", commit.clone())
                .with_detail("subject", subject.clone())
                .with_detail("kind", kind.as_str())
                .with_detail("detail", detail.clone()),
            RebaseError::CommitLoad { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
            }
            RebaseError::OriginalCommitLoad { .. } | RebaseError::OriginalTreeLoad { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
            }
            RebaseError::BranchRestore { .. }
            | RebaseError::TreeCreate(..)
            | RebaseError::CommitSave(..)
            | RebaseError::IndexRebuild(..)
            | RebaseError::IndexSave(..)
            | RebaseError::WorkdirReset(..)
            | RebaseError::StateSave(..)
            | RebaseError::AuxStateSave { .. }
            | RebaseError::Finalize(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            RebaseError::IndexLoad(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
        }
    }
}

/// Execute the rebase command
///
/// Rebase moves or combines a sequence of commits to a new base commit.
/// This implementation performs a linear rebase by:
/// 1. Finding the common ancestor between current branch and upstream
/// 2. Collecting all commits from the common ancestor to current HEAD
/// 3. Replaying each commit on top of the upstream branch
/// 4. Updating the current branch reference to point to the final commit
///
/// The process maintains commit order but changes their parent relationships,
/// effectively "moving" the branch to start from the upstream commit.
pub async fn execute(args: RebaseArgs) {
    if let Err(error) = execute_safe(args, &OutputConfig::default()).await {
        error.print_stderr();
    }
}

/// Safe CLI entry point with preflight validation for argument and state errors.
pub async fn execute_safe(args: RebaseArgs, output: &OutputConfig) -> CliResult<()> {
    crate::command::ensure_main_worktree("rebase")?;
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Refuse to start a NEW rebase while a cherry-pick sequence is in progress
    // (rebase's own --continue/--abort/--skip operate on rebase state, not
    // cherry-pick, so they are exempt from this guard).
    if !(args.continue_rebase || args.abort || args.skip) {
        crate::internal::sequencer::ensure_none_in_progress(
            crate::internal::sequencer::SequenceKind::Rebase,
        )
        .await?;
    }

    // For --continue, --abort, --skip: verify that a rebase is actually in
    // progress before delegating to typed runners.  This ensures
    // a non-zero exit code (128) is returned when there is nothing to do,
    // matching the behaviour of `git rebase --abort` / `--continue` / `--skip`.
    if args.continue_rebase || args.abort || args.skip {
        match RebaseState::is_in_progress().await {
            Ok(true) => { /* rebase in progress – proceed */ }
            Ok(false) => {
                let verb = if args.abort {
                    "abort"
                } else if args.skip {
                    "skip"
                } else {
                    "continue"
                };
                return Err(CliError::fatal("no rebase in progress")
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint(format!(
                        "cannot --{verb} because there is no rebase in progress."
                    )));
            }
            Err(err) => {
                return Err(
                    CliError::fatal(format!("failed to check rebase state: {err}"))
                        .with_stable_code(StableErrorCode::IoReadFailed),
                );
            }
        }
    }

    preflight_rebase(&args).await?;
    // Validate `--empty` before any dispatch (start or sequencer control) so a bad
    // mode fails fast (exit 129) rather than slipping through.
    let empty_mode = resolve_rebase_empty_mode(&args)?;
    if args.abort {
        let result = run_rebase_abort().await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if args.continue_rebase {
        let result = run_rebase_continue().await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if args.skip {
        let result = run_rebase_skip().await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if let Some(upstream) = args.upstream.as_deref() {
        prepare_rebase_aux(&args).await.map_err(CliError::from)?;
        // `git rebase --onto <newbase> <upstream> <branch>` form: check out the
        // named branch first (no-op when it is already current), so the rest of
        // the start path rebases it as "the current branch".
        let start_result = async {
            if let Some(branch) = args.branch.as_deref() {
                switch_to_rebase_branch(branch, output).await?;
            }
            run_rebase_start(
                upstream,
                args.onto.as_deref(),
                args.autosquash,
                args.reapply_cherry_picks,
                args.no_keep_empty,
                empty_mode,
                args.fork_point,
            )
            .await
            .map_err(CliError::from)
        }
        .await;

        let in_progress = RebaseState::is_in_progress()
            .await
            .map_err(|detail| CliError::from(RebaseError::StateCheck(detail)))?;
        if !in_progress {
            resolve_rebase_autostash().await.map_err(CliError::from)?;
            RebaseAuxState::cleanup().map_err(CliError::from)?;
        }
        let result = start_result?;
        return render_rebase_output(&result, output);
    }
    Ok(())
}

/// Check out `<branch>` before a `rebase ... <branch>` start, unless it is
/// already the current branch. Uses `switch::execute_safe` (not `execute`) so a
/// switch failure (dirty worktree, missing branch) propagates as a non-zero
/// exit / structured error instead of being swallowed.
async fn switch_to_rebase_branch(branch: &str, output: &OutputConfig) -> CliResult<()> {
    if let Head::Branch(current) = Head::current().await
        && current == branch
    {
        return Ok(());
    }
    switch::execute_safe(
        switch::SwitchArgs {
            no_progress: false,
            branch: Some(branch.to_string()),
            create: None,
            force_create: None,
            orphan: None,
            detach: false,
            track: false,
            force: false,
            guess: false,
            no_guess: false,
        },
        output,
    )
    .await
}

fn render_rebase_output(result: &RebaseOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("rebase", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    if result.action == "start" {
        render_rebase_start_output(result);
        return Ok(());
    }

    if result.action == "abort" {
        println!("Rebase aborted. Restored branch '{}'.", result.branch);
        return Ok(());
    }

    if result.action == "skip" {
        let skipped_commit = result
            .skipped_commit
            .as_deref()
            .map(short_id)
            .unwrap_or_else(|| "unknown".to_string());
        if let Some(subject) = result.skipped_subject.as_deref() {
            println!("Skipped: {skipped_commit} {subject}");
        } else {
            println!("Skipped: {skipped_commit} (message unavailable)");
        }
    }

    for dropped in &result.dropped_commits {
        println!(
            "dropping {} {} -- patch contents already upstream",
            dropped.commit, dropped.subject
        );
    }
    for applied in &result.applied_commits {
        println!("Applied: {} {}", short_id(&applied.commit), applied.subject);
    }

    if matches!(result.action.as_str(), "continue" | "skip") && result.status == "completed" {
        let onto = result.onto.as_deref().unwrap_or(&result.commit);
        println!(
            "Successfully rebased branch '{}' onto '{}'.",
            result.branch,
            short_id(onto)
        );
    }
    Ok(())
}

fn render_rebase_start_output(result: &RebaseOutput) {
    let upstream = result
        .upstream
        .as_deref()
        .or(result.onto.as_deref())
        .unwrap_or(&result.commit);

    match result.status.as_str() {
        "fast-forwarded" => {
            println!(
                "Fast-forwarded branch '{}' to '{}'.",
                result.branch, upstream
            );
        }
        "already-up-to-date" => {
            println!("Current branch is ahead of upstream. No rebase needed.");
        }
        "no-commits" => {
            println!("No commits to rebase on branch '{}'.", result.branch);
        }
        _ => {
            if let Some(common_ancestor) = result.common_ancestor.as_deref() {
                println!("Found common ancestor: {}", short_id(common_ancestor));
            }
            if let Some(replay_count) = result.replay_count {
                println!(
                    "Rebasing {replay_count} commits from `{}` onto `{upstream}`...",
                    result.branch
                );
            }
            for dropped in &result.dropped_commits {
                println!(
                    "dropping {} {} -- patch contents already upstream",
                    dropped.commit, dropped.subject
                );
            }
            for applied in &result.applied_commits {
                println!("Applied: {} {}", short_id(&applied.commit), applied.subject);
            }
            println!(
                "Successfully rebased branch '{}' onto '{}'.",
                result.branch,
                short_id(&result.commit)
            );
        }
    }
}

async fn ensure_rebase_in_progress() -> Result<(), RebaseError> {
    match RebaseState::is_in_progress().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(RebaseError::NoRebaseInProgress),
        Err(e) => Err(RebaseError::StateCheck(e)),
    }
}

fn short_id(value: &str) -> String {
    value.chars().take(7).collect()
}

fn short_object_id(value: &ObjectHash) -> String {
    short_id(&value.to_string())
}

fn commit_subject_from_message(message: &str) -> String {
    parse_commit_msg(message)
        .0
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn commit_subject_lossy(commit_id: &ObjectHash, emit_human: bool) -> String {
    match load_object::<Commit>(commit_id) {
        Ok(commit) => commit_subject_from_message(&commit.message),
        Err(e) => {
            if emit_human {
                cli_error!(
                    e,
                    "warning: failed to load commit {}",
                    short_object_id(commit_id)
                );
            }
            "unknown".to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseTodoAction {
    Pick,
    Fixup,
    Squash,
    Amend,
}

impl RebaseTodoAction {
    fn from_message(message: &str) -> Self {
        let subject = commit_subject_from_message(message);
        if subject.starts_with("fixup! ") {
            Self::Fixup
        } else if subject.starts_with("squash! ") {
            Self::Squash
        } else if subject.starts_with("amend! ") {
            Self::Amend
        } else {
            Self::Pick
        }
    }

    fn from_token(value: &str) -> Result<Self, String> {
        match value {
            "pick" => Ok(Self::Pick),
            "fixup" => Ok(Self::Fixup),
            "squash" => Ok(Self::Squash),
            "amend" => Ok(Self::Amend),
            other => Err(format!("invalid rebase todo action '{other}'")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Fixup => "fixup",
            Self::Squash => "squash",
            Self::Amend => "amend",
        }
    }

    fn folds_into_previous(self) -> bool {
        matches!(self, Self::Fixup | Self::Squash | Self::Amend)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RebaseTodoItem {
    commit: ObjectHash,
    action: RebaseTodoAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutosquashTodoItem {
    item: RebaseTodoItem,
    original_index: usize,
}

fn autosquash_commits(commits: Vec<ObjectHash>) -> Result<Vec<RebaseTodoItem>, RebaseError> {
    let mut picks = Vec::new();
    let mut fixups = Vec::new();

    for (original_index, commit_id) in commits.into_iter().enumerate() {
        let commit: Commit = load_object(&commit_id).map_err(|error| RebaseError::CommitLoad {
            commit: commit_id.to_string(),
            detail: error.to_string(),
        })?;
        let action = RebaseTodoAction::from_message(&commit.message);
        if action.folds_into_previous() {
            fixups.push(AutosquashTodoItem {
                item: RebaseTodoItem {
                    commit: commit_id,
                    action,
                },
                original_index,
            });
        } else {
            picks.push(AutosquashTodoItem {
                item: RebaseTodoItem {
                    commit: commit_id,
                    action: RebaseTodoAction::Pick,
                },
                original_index,
            });
        }
    }

    for fixup in fixups {
        let fixup_commit: Commit =
            load_object(&fixup.item.commit).map_err(|error| RebaseError::CommitLoad {
                commit: fixup.item.commit.to_string(),
                detail: error.to_string(),
            })?;
        let Some(target) = autosquash_target(&fixup_commit.message) else {
            insert_pick_by_original_index(
                &mut picks,
                AutosquashTodoItem {
                    item: RebaseTodoItem {
                        commit: fixup.item.commit,
                        action: RebaseTodoAction::Pick,
                    },
                    original_index: fixup.original_index,
                },
            );
            continue;
        };
        let Some(target_pos) = autosquash_target_position(&picks, fixup.original_index, &target)
        else {
            insert_pick_by_original_index(
                &mut picks,
                AutosquashTodoItem {
                    item: RebaseTodoItem {
                        commit: fixup.item.commit,
                        action: RebaseTodoAction::Pick,
                    },
                    original_index: fixup.original_index,
                },
            );
            continue;
        };

        let mut insert_at = target_pos + 1;
        while insert_at < picks.len() {
            if picks[insert_at].item.action.folds_into_previous() {
                insert_at += 1;
            } else {
                break;
            }
        }
        picks.insert(insert_at, fixup);
    }

    Ok(picks.into_iter().map(|entry| entry.item).collect())
}

fn insert_pick_by_original_index(picks: &mut Vec<AutosquashTodoItem>, item: AutosquashTodoItem) {
    let insert_at = picks
        .iter()
        .position(|candidate| candidate.original_index > item.original_index)
        .unwrap_or(picks.len());
    picks.insert(insert_at, item);
}

fn autosquash_target(message: &str) -> Option<String> {
    let mut subject = commit_subject_from_message(message);
    let mut peeled = false;

    while let Some(target) = autosquash_target_once(&subject) {
        if target.is_empty() {
            return None;
        }
        subject = target.to_string();
        peeled = true;
    }

    peeled.then_some(subject)
}

fn autosquash_target_once(subject: &str) -> Option<&str> {
    for prefix in ["fixup! ", "squash! ", "amend! "] {
        if let Some(target) = subject.strip_prefix(prefix) {
            return Some(target.trim());
        }
    }
    None
}

fn autosquash_target_position(
    picks: &[AutosquashTodoItem],
    fixup_original_index: usize,
    target: &str,
) -> Option<usize> {
    let mut prefix_match = None;
    for (index, candidate) in picks.iter().enumerate() {
        if candidate.original_index >= fixup_original_index {
            continue;
        }
        match autosquash_target_match_kind(&candidate.item.commit, target) {
            Some(AutosquashTargetMatch::Exact) => return Some(index),
            Some(AutosquashTargetMatch::Prefix) if prefix_match.is_none() => {
                prefix_match = Some(index);
            }
            _ => {}
        }
    }
    prefix_match
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutosquashTargetMatch {
    Exact,
    Prefix,
}

fn autosquash_target_match_kind(
    commit_id: &ObjectHash,
    target: &str,
) -> Option<AutosquashTargetMatch> {
    let full = commit_id.to_string();
    if full.starts_with(target) {
        return Some(AutosquashTargetMatch::Exact);
    }
    load_object::<Commit>(commit_id)
        .map(|commit| {
            let subject = commit_subject_from_message(&commit.message);
            if subject == target {
                Some(AutosquashTargetMatch::Exact)
            } else if subject.starts_with(target) {
                Some(AutosquashTargetMatch::Prefix)
            } else {
                None
            }
        })
        .unwrap_or(None)
}

async fn preflight_rebase(args: &RebaseArgs) -> CliResult<()> {
    if args.continue_rebase || args.abort || args.skip {
        return Ok(());
    }

    let upstream = args
        .upstream
        .as_deref()
        .ok_or_else(|| CliError::fatal("no upstream specified"))?;

    match RebaseState::is_in_progress().await {
        Ok(true) => {
            return Err(CliError::fatal("rebase already in progress")
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use 'libra rebase --continue' to continue rebasing.")
                .with_hint(
                    "use 'libra rebase --abort' to abort and restore the original branch.",
                ));
        }
        Ok(false) => {}
        Err(err) => {
            return Err(
                CliError::fatal(format!("failed to check rebase state: {err}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
    }

    // `resolve_branch_or_commit` returns legacy `"fatal: ..."` prefixed strings,
    // so `from_legacy_string` strips the prefix to avoid double-prefix rendering.
    resolve_branch_or_commit(upstream)
        .await
        .map_err(CliError::from_legacy_string)?;

    // Pre-resolve the --onto target so an unresolvable newbase fails fast,
    // before any worktree/state mutation (run_rebase_start re-resolves it for
    // the typed `OntoResolve` error).
    if let Some(onto) = args.onto.as_deref() {
        resolve_branch_or_commit(onto)
            .await
            .map_err(CliError::from_legacy_string)?;
    }
    Ok(())
}

fn validate_exec_commands(commands: &[String]) -> Result<(), RebaseError> {
    for command in commands {
        if command.trim().is_empty() {
            return Err(RebaseError::InvalidExec(
                "command must not be empty".to_string(),
            ));
        }
        if command.contains('\0') {
            return Err(RebaseError::InvalidExec(
                "command must not contain a NUL byte".to_string(),
            ));
        }
    }
    Ok(())
}

/// Recover an auxiliary sidecar left after the primary rebase state was
/// already removed. A held stash is promoted into the normal stash list before
/// the stale file is discarded, so a crash can duplicate changes but never
/// lose them.
async fn recover_stale_rebase_aux() -> Result<(), RebaseError> {
    if RebaseState::is_in_progress()
        .await
        .map_err(RebaseError::StateCheck)?
    {
        return Ok(());
    }
    let Some(aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if let Some(stash) = aux.autostash {
        let oid = ObjectHash::from_str(&stash).map_err(|error| {
            RebaseError::Autostash(format!(
                "rebase-aux.json contains invalid stash object '{stash}': {error}"
            ))
        })?;
        crate::command::stash::store_stash_commit(&oid, "autostash")
            .await
            .map_err(|error| {
                RebaseError::Autostash(format!(
                    "failed to recover held stash {stash} into the stash list: {error}"
                ))
            })?;
        emit_warning(
            "recovered a stale rebase autostash into the stash list; inspect it with 'libra stash show'",
        );
    }
    RebaseAuxState::cleanup()
}

async fn prepare_rebase_aux(args: &RebaseArgs) -> Result<(), RebaseError> {
    validate_exec_commands(&args.exec)?;
    recover_stale_rebase_aux().await?;

    let mut aux = RebaseAuxState {
        exec_commands: args.exec.clone(),
        update_refs: args.update_refs,
        ..Default::default()
    };
    if args.autostash {
        match crate::command::stash::create_held_stash_commit("autostash").await {
            Ok(Some(stash)) => {
                aux.autostash = Some(stash.to_string());
                // ORDER IS LOAD-BEARING: stash object -> durable sidecar ->
                // destructive reset. A crash never leaves the dirty data both
                // absent from the worktree and unreachable.
                aux.save()?;
                crate::command::stash::reset_to_head_for_held_stash()
                    .await
                    .map_err(|error| {
                        RebaseError::Autostash(format!(
                            "created stash {stash} but failed to clean the worktree: {error}; rebase-aux.json still references it"
                        ))
                    })?;
            }
            Ok(None) => {}
            Err(error) => return Err(RebaseError::Autostash(error.to_string())),
        }
    }
    if !aux.exec_commands.is_empty() || aux.update_refs || aux.autostash.is_some() {
        aux.save()?;
    }
    Ok(())
}

async fn resolve_rebase_autostash() -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    let Some(stash) = aux.autostash.take() else {
        return Ok(());
    };
    let oid = ObjectHash::from_str(&stash).map_err(|error| {
        RebaseError::Autostash(format!(
            "rebase-aux.json contains invalid stash object '{stash}': {error}"
        ))
    })?;
    match crate::command::stash::apply_held_stash_commit(&oid).await {
        Ok(()) => {
            aux.save()?;
            Ok(())
        }
        Err(apply_error) => {
            crate::command::stash::store_stash_commit(&oid, "autostash")
                .await
                .map_err(|store_error| {
                    RebaseError::Autostash(format!(
                        "could not re-apply stash {stash} ({apply_error}) and could not preserve it in the stash list ({store_error}); rebase-aux.json still references it"
                    ))
                })?;
            aux.save()?;
            emit_warning(format!(
                "rebase completed, but autostash re-apply conflicted ({apply_error}); changes are safe in stash@{{0}}"
            ));
            Ok(())
        }
    }
}

async fn checked_out_local_branches() -> Result<HashSet<String>, RebaseError> {
    let db = get_db_conn_instance().await;
    ref_model::Entity::find()
        .filter(ref_model::Column::Kind.eq(ref_model::ConfigKind::Head))
        .filter(ref_model::Column::Remote.is_null())
        .all(&db)
        .await
        .map_err(|error| {
            RebaseError::UpdateRefs(format!(
                "failed to inspect branches checked out by repository worktrees: {error}"
            ))
        })
        .map(|heads| heads.into_iter().filter_map(|head| head.name).collect())
}

async fn capture_rebase_update_refs(
    commits: &[ObjectHash],
    current_branch: &str,
) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if !aux.update_refs {
        return Ok(());
    }
    let rewritten = commits.iter().copied().collect::<HashSet<_>>();
    let checked_out = checked_out_local_branches().await?;
    let branches = Branch::list_branches_result(None).await.map_err(|error| {
        RebaseError::UpdateRefs(format!("failed to list local branches: {error}"))
    })?;
    aux.refs_to_update = branches
        .into_iter()
        .filter(|branch| {
            branch.name != current_branch
                && !checked_out.contains(&branch.name)
                && rewritten.contains(&branch.commit)
        })
        .map(|branch| RebaseRefUpdate {
            branch: branch.name,
            old_oid: branch.commit.to_string(),
        })
        .collect();
    aux.refs_to_update
        .sort_by(|left, right| left.branch.cmp(&right.branch));
    aux.save()
}

fn record_start_empty_rewrite_aliases(
    original_range: &[ObjectHash],
    retained: &[ObjectHash],
    newbase: ObjectHash,
) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if !aux.update_refs {
        return Ok(());
    }
    let originals = original_range.iter().copied().collect::<HashSet<_>>();
    let retained = retained.iter().copied().collect::<HashSet<_>>();
    for commit_id in original_range {
        if retained.contains(commit_id) {
            continue;
        }
        let commit: Commit = load_object(commit_id).map_err(|error| RebaseError::CommitLoad {
            commit: commit_id.to_string(),
            detail: format!("recording --update-refs empty-commit mapping: {error}"),
        })?;
        let target = commit
            .parent_commit_ids
            .first()
            .copied()
            .filter(|parent| originals.contains(parent))
            .unwrap_or(newbase);
        aux.rewrite_aliases
            .insert(commit_id.to_string(), target.to_string());
    }
    aux.save()
}

fn resolve_rebase_rewrite(
    aux: &RebaseAuxState,
    original: &str,
    newbase: ObjectHash,
) -> anyhow::Result<ObjectHash> {
    let mut current = original;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.to_string()) {
            anyhow::bail!("rebase update-refs rewrite mapping contains a cycle at {current}");
        }
        if let Some(rewritten) = aux.rewrites.get(current) {
            return ObjectHash::from_str(rewritten)
                .map_err(anyhow::Error::msg)
                .context("rebase update-refs recorded an invalid rewritten object");
        }
        if current == newbase.to_string() {
            return Ok(newbase);
        }
        current = aux.rewrite_aliases.get(current).with_context(|| {
            format!("rebase update-refs has no rewrite recorded for commit {current}")
        })?;
    }
}

fn record_rebase_rewrite(
    original: ObjectHash,
    previous_tip: ObjectHash,
    rewritten: ObjectHash,
    folds_previous: bool,
) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if folds_previous {
        let previous = previous_tip.to_string();
        for target in aux.rewrites.values_mut() {
            if *target == previous {
                *target = rewritten.to_string();
            }
        }
    }
    aux.rewrites
        .insert(original.to_string(), rewritten.to_string());
    aux.save()
}

async fn run_sandboxed_rebase_exec(
    command: &str,
) -> Result<crate::internal::ai::sandbox::SandboxExecOutput, String> {
    use crate::internal::ai::sandbox::{
        NetworkAccess, SandboxEnforcement, SandboxPermissions, SandboxPolicy, SandboxRuntimeConfig,
        ToolSandboxContext, run_shell_command,
    };

    let cwd = util::working_dir();
    let sandbox = ToolSandboxContext {
        policy: SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.clone()],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        permissions: SandboxPermissions::UseDefault,
    };
    let runtime = SandboxRuntimeConfig {
        enforcement: SandboxEnforcement::Required,
        use_linux_sandbox_bwrap: true,
        ..Default::default()
    };
    run_shell_command(
        command,
        &cwd,
        Some(15 * 60 * 1000),
        1024 * 1024,
        Some(sandbox),
        Some(&runtime),
    )
    .await
}

async fn run_pending_rebase_exec(state: &mut RebaseState) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    let Some(mut index) = aux.pending_exec else {
        return Ok(());
    };
    while index < aux.exec_commands.len() {
        let command = aux.exec_commands[index].clone();
        aux.pending_exec = Some(index);
        aux.save()?;
        let result = run_sandboxed_rebase_exec(&command)
            .await
            .map_err(|detail| RebaseError::ExecFailed {
                commit: state.current_head.to_string(),
                command: command.clone(),
                exit_code: -1,
                detail: format!(": {detail}"),
            })?;
        reconcile_rebase_exec_head(state, &mut aux).await?;
        if result.exit_code != 0 || result.timed_out {
            let detail_text = if !result.stderr.trim().is_empty() {
                result.stderr.trim()
            } else {
                result.stdout.trim()
            };
            let detail = if result.timed_out && detail_text.is_empty() {
                ": command timed out after 900 seconds".to_string()
            } else if detail_text.is_empty() {
                String::new()
            } else {
                format!(": {detail_text}")
            };
            return Err(RebaseError::ExecFailed {
                commit: state.current_head.to_string(),
                command,
                exit_code: result.exit_code,
                detail,
            });
        }
        let quiet_output = OutputConfig {
            quiet: true,
            ..Default::default()
        };
        switch::ensure_clean_status(&quiet_output)
            .await
            .map_err(|error| RebaseError::ExecFailed {
                commit: state.current_head.to_string(),
                command: command.clone(),
                exit_code: 0,
                detail: format!(": command left tracked changes: {error}"),
            })?;
        index += 1;
        aux.pending_exec = (index < aux.exec_commands.len()).then_some(index);
        aux.save()?;
    }

    Ok(())
}

async fn reconcile_rebase_exec_head(
    state: &mut RebaseState,
    aux: &mut RebaseAuxState,
) -> Result<(), RebaseError> {
    let actual_tip = Head::current_commit()
        .await
        .ok_or_else(|| RebaseError::ExecFailed {
            commit: state.current_head.to_string(),
            command: "<post-exec HEAD check>".to_string(),
            exit_code: 0,
            detail: ": command left HEAD unborn".to_string(),
        })?;
    if actual_tip != state.current_head {
        let previous = state.current_head.to_string();
        for target in aux.rewrites.values_mut() {
            if *target == previous {
                *target = actual_tip.to_string();
            }
        }
        aux.save()?;
        state.current_head = actual_tip;
        state.save().await.map_err(RebaseError::StateSave)?;
    }
    Ok(())
}

async fn schedule_rebase_exec(state: &mut RebaseState) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if aux.exec_commands.is_empty() {
        return Ok(());
    }
    aux.pending_exec = Some(0);
    aux.save()?;
    run_pending_rebase_exec(state).await
}

async fn upstream_reflog_name(upstream: &str) -> Result<Option<String>, RebaseError> {
    if upstream.starts_with("refs/heads/") || upstream.starts_with("refs/remotes/") {
        return Ok(Some(upstream.to_string()));
    }
    if Branch::find_branch_result(upstream, None)
        .await
        .map_err(|error| {
            RebaseError::StateLoad(format!(
                "failed to resolve --fork-point upstream reflog: {error}"
            ))
        })?
        .is_some()
    {
        return Ok(Some(format!("refs/heads/{upstream}")));
    }
    let matches = Branch::search_branch_result(upstream)
        .await
        .map_err(|error| {
            RebaseError::StateLoad(format!(
                "failed to resolve --fork-point upstream reflog: {error}"
            ))
        })?;
    Ok(matches.into_iter().find_map(|branch| {
        branch
            .remote
            .map(|remote| format!("refs/remotes/{remote}/{}", branch.name))
    }))
}

async fn reflog_fork_point(
    upstream: &str,
    upstream_id: ObjectHash,
    head: ObjectHash,
) -> Result<Option<ObjectHash>, RebaseError> {
    let Some(ref_name) = upstream_reflog_name(upstream).await? else {
        return Ok(None);
    };
    let db = get_db_conn_instance().await;
    let entries = reflog_model::Entity::find()
        .filter(reflog_model::Column::RefName.eq(ref_name))
        .order_by_desc(reflog_model::Column::Timestamp)
        .order_by_desc(reflog_model::Column::Id)
        .all(&db)
        .await
        .map_err(|error| {
            RebaseError::StateLoad(format!(
                "failed to read upstream reflog for --fork-point: {error}"
            ))
        })?;
    let mut candidates = vec![upstream_id];
    for entry in entries {
        for value in [entry.new_oid, entry.old_oid] {
            if let Ok(candidate) = ObjectHash::from_str(&value) {
                candidates.push(candidate);
            }
        }
    }
    let mut seen = HashSet::new();
    let mut best = None;
    for candidate in candidates {
        if !seen.insert(candidate) {
            continue;
        }
        let is_ancestor =
            crate::internal::merge_base::is_ancestor(&candidate, &head).map_err(|error| {
                RebaseError::CommitLoad {
                    commit: candidate.to_string(),
                    detail: format!("checking --fork-point ancestry: {error}"),
                }
            })?;
        if is_ancestor {
            let replace = match best {
                None => true,
                Some(current) => crate::internal::merge_base::is_ancestor(&current, &candidate)
                    .map_err(|error| RebaseError::CommitLoad {
                        commit: candidate.to_string(),
                        detail: format!("ranking --fork-point candidates: {error}"),
                    })?,
            };
            if replace {
                best = Some(candidate);
            }
        }
    }
    Ok(best)
}

async fn run_rebase_start(
    upstream: &str,
    onto: Option<&str>,
    autosquash: bool,
    _reapply_cherry_picks: bool,
    no_keep_empty: bool,
    empty_mode: RebaseEmptyMode,
    fork_point: bool,
) -> Result<RebaseOutput, RebaseError> {
    let db = get_db_conn_instance().await;

    let current_branch_name = match Head::current().await {
        Head::Branch(name) if !name.is_empty() => name,
        _ => return Err(RebaseError::NotOnBranch),
    };

    let head_to_rebase_id =
        Head::current_commit()
            .await
            .ok_or_else(|| RebaseError::BranchHasNoCommits {
                branch: current_branch_name.clone(),
            })?;

    let upstream_id = resolve_branch_or_commit(upstream).await.map_err(|detail| {
        RebaseError::UpstreamResolve {
            upstream: upstream.to_string(),
            detail,
        }
    })?;

    // The landing point ("onto") defaults to the upstream when --onto is absent,
    // so existing behaviour is unchanged. With --onto, the replayed range stays
    // <upstream>..HEAD (computed from `upstream_id`) but the commits land on
    // `newbase_id` instead.
    let newbase_id =
        match onto {
            Some(target) => resolve_branch_or_commit(target).await.map_err(|detail| {
                RebaseError::OntoResolve {
                    onto: target.to_string(),
                    detail,
                }
            })?,
            None => upstream_id,
        };

    // The true lowest common ancestor (shared with `merge-base` / `diff A...B`),
    // not a first-meet BFS — correct for criss-cross histories.
    let ordinary_base = crate::internal::merge_base::merge_base(&head_to_rebase_id, &upstream_id)
        .map_err(|error| RebaseError::CommitLoad {
            commit: head_to_rebase_id.to_string(),
            detail: format!("computing merge base with {upstream_id}: {error}"),
        })?
        .ok_or(RebaseError::NoCommonAncestor)?;
    let base_id = if fork_point {
        reflog_fork_point(upstream, upstream_id, head_to_rebase_id)
            .await?
            .unwrap_or(ordinary_base)
    } else {
        ordinary_base
    };

    // Fast-forward and already-up-to-date short-circuits apply only to a plain
    // rebase (no explicit --onto). With --onto, an explicit landing point must
    // always replay <upstream>..HEAD onto <newbase>, even when upstream is an
    // ancestor of HEAD (range non-empty) — otherwise the commits would never be
    // moved onto the new base.
    if onto.is_none() && base_id == head_to_rebase_id {
        let upstream_commit: Commit =
            load_object(&upstream_id).map_err(|e| RebaseError::CommitLoad {
                commit: upstream_id.to_string(),
                detail: e.to_string(),
            })?;
        let upstream_tree: Tree =
            load_object(&upstream_commit.tree_id).map_err(|e| RebaseError::OriginalTreeLoad {
                tree: upstream_commit.tree_id.to_string(),
                detail: e.to_string(),
            })?;

        let index_file = path::index();
        let current_index = git_internal::internal::index::Index::load(&index_file)
            .map_err(|e| RebaseError::IndexLoad(e.to_string()))?;
        let mut index = git_internal::internal::index::Index::new();
        rebuild_index_from_tree(&upstream_tree, &mut index, "")
            .map_err(RebaseError::IndexRebuild)?;
        rebase_worktree_guard_structured(&index, "fast-forward rebase").await?;

        let fast_forward_action = ReflogAction::Rebase {
            state: "fast-forward".to_string(),
            details: format!("moving {} to {}", current_branch_name, upstream),
        };
        let fast_forward_context = ReflogContext {
            old_oid: head_to_rebase_id.to_string(),
            new_oid: upstream_id.to_string(),
            action: fast_forward_action,
        };

        let branch_name_cloned = current_branch_name.clone();
        let upstream_id_str = upstream_id.to_string();
        with_reflog(
            fast_forward_context,
            move |txn: &sea_orm::DatabaseTransaction| {
                Box::pin(async move {
                    Branch::update_branch_with_conn(
                        txn,
                        &branch_name_cloned,
                        &upstream_id_str,
                        None,
                    )
                    .await?;
                    Head::update_with_conn(txn, Head::Branch(branch_name_cloned), None).await;
                    Ok(())
                })
            },
            true,
        )
        .await
        .map_err(|e| RebaseError::Finalize(format!("failed to fast-forward: {e}")))?;

        index
            .save(&index_file)
            .map_err(|e| RebaseError::IndexSave(e.to_string()))?;
        reset_workdir_tracked_only(&current_index, &index).map_err(RebaseError::WorkdirReset)?;

        return Ok(RebaseOutput {
            action: "start".to_string(),
            status: "fast-forwarded".to_string(),
            branch: current_branch_name,
            commit: upstream_id.to_string(),
            upstream: Some(upstream.to_string()),
            onto: Some(upstream_id.to_string()),
            common_ancestor: Some(base_id.to_string()),
            replay_count: Some(0),
            previous_commit: Some(head_to_rebase_id.to_string()),
            restored: None,
            applied_commits: Vec::new(),
            dropped_commits: Vec::new(),
            skipped_commit: None,
            skipped_subject: None,
            remaining: Some(0),
        });
    }

    if onto.is_none() && base_id == upstream_id {
        return Ok(RebaseOutput {
            action: "start".to_string(),
            status: "already-up-to-date".to_string(),
            branch: current_branch_name,
            commit: head_to_rebase_id.to_string(),
            upstream: Some(upstream.to_string()),
            onto: Some(upstream_id.to_string()),
            common_ancestor: Some(base_id.to_string()),
            replay_count: Some(0),
            previous_commit: Some(head_to_rebase_id.to_string()),
            restored: None,
            applied_commits: Vec::new(),
            dropped_commits: Vec::new(),
            skipped_commit: None,
            skipped_subject: None,
            remaining: Some(0),
        });
    }

    let mut commits_to_replay = collect_commits_to_replay(&base_id, &head_to_rebase_id)
        .await
        .map_err(|detail| RebaseError::CommitLoad {
            commit: head_to_rebase_id.to_string(),
            detail,
        })?;
    let original_commits_to_replay = commits_to_replay.clone();
    // `--no-keep-empty`: drop commits that are ALREADY empty in the original
    // history (their tree equals their first parent's tree — i.e. they introduce
    // no change). Filtering the replay list up front means the persisted todo is
    // already pruned, so `--continue` honors it without extra state. (Commits that
    // only BECOME empty after replay are a separate concept — `--empty=drop` — and
    // are not handled here.)
    //
    // `had_commits_before_filter` distinguishes "nothing to rebase" (collect
    // returned empty — head is already on/behind the base) from "everything was an
    // empty commit we just dropped". In the latter case the branch must still be
    // moved to the new base, so the early no-commits return below is skipped.
    let had_commits_before_filter = !commits_to_replay.is_empty();
    if no_keep_empty {
        let mut kept = Vec::with_capacity(commits_to_replay.len());
        for commit_id in commits_to_replay {
            if !commit_starts_empty(&commit_id).await {
                kept.push(commit_id);
            }
        }
        commits_to_replay = kept;
    }
    let mut todo_actions = VecDeque::from(vec![RebaseTodoAction::Pick; commits_to_replay.len()]);
    if autosquash {
        let planned_todo = autosquash_commits(commits_to_replay)?;
        commits_to_replay = planned_todo.iter().map(|item| item.commit).collect();
        todo_actions = planned_todo.iter().map(|item| item.action).collect();
    }
    // Only genuinely-nothing-to-rebase (collect returned empty) returns early and
    // leaves the branch put. If `--no-keep-empty` emptied a non-empty range, fall
    // through to the normal setup so the branch is still rebased onto newbase
    // (replaying zero commits) — otherwise the dropped empties would silently stay.
    if commits_to_replay.is_empty() && !had_commits_before_filter {
        return Ok(RebaseOutput {
            action: "start".to_string(),
            status: "no-commits".to_string(),
            branch: current_branch_name,
            commit: head_to_rebase_id.to_string(),
            upstream: Some(upstream.to_string()),
            onto: Some(newbase_id.to_string()),
            common_ancestor: Some(base_id.to_string()),
            replay_count: Some(0),
            previous_commit: Some(head_to_rebase_id.to_string()),
            restored: None,
            applied_commits: Vec::new(),
            dropped_commits: Vec::new(),
            skipped_commit: None,
            skipped_subject: None,
            remaining: Some(0),
        });
    }

    capture_rebase_update_refs(&original_commits_to_replay, &current_branch_name).await?;
    record_start_empty_rewrite_aliases(
        &original_commits_to_replay,
        &commits_to_replay,
        newbase_id,
    )?;

    // Build the worktree guard against the LANDING (newbase) tree, since the
    // start detaches HEAD onto `newbase_id` before replaying. For a plain rebase
    // `newbase_id == upstream_id`, so this is unchanged there.
    let newbase_commit: Commit = load_object(&newbase_id).map_err(|e| RebaseError::CommitLoad {
        commit: newbase_id.to_string(),
        detail: e.to_string(),
    })?;
    let newbase_tree: Tree =
        load_object(&newbase_commit.tree_id).map_err(|e| RebaseError::OriginalTreeLoad {
            tree: newbase_commit.tree_id.to_string(),
            detail: e.to_string(),
        })?;
    let mut guard_index = git_internal::internal::index::Index::new();
    rebuild_index_from_tree(&newbase_tree, &mut guard_index, "")
        .map_err(RebaseError::IndexRebuild)?;
    rebase_worktree_guard_structured(&guard_index, "rebase").await?;

    // The replay lands on `newbase_id` (== upstream_id for a plain rebase): the
    // initial detach, the rebase state's onto/current_head, and the start reflog
    // all point at the landing commit, while the replayed range was computed
    // from `upstream`.
    let landing_display = onto.unwrap_or(upstream);
    let start_action = ReflogAction::Rebase {
        state: "start".to_string(),
        details: format!("checkout {}", landing_display),
    };
    let start_context = ReflogContext {
        old_oid: head_to_rebase_id.to_string(),
        new_oid: newbase_id.to_string(),
        action: start_action,
    };
    db.transaction(|txn| {
        Box::pin(async move {
            reflog::Reflog::insert_single_entry(txn, &start_context, "HEAD").await?;
            Head::update_with_conn(txn, Head::Detached(newbase_id), None).await;
            Ok::<_, ReflogError>(())
        })
    })
    .await
    .map_err(|e| RebaseError::Finalize(format!("failed to start rebase: {e}")))?;

    let replay_count = commits_to_replay.len();
    let mut state = RebaseState {
        head_name: current_branch_name.clone(),
        onto: newbase_id,
        orig_head: head_to_rebase_id,
        todo: VecDeque::from(commits_to_replay),
        todo_actions,
        done: Vec::new(),
        stopped_sha: None,
        current_head: newbase_id,
        autosquash,
        empty_mode,
    };

    state.save().await.map_err(RebaseError::StateSave)?;
    Head::update_with_conn(&db, Head::Detached(newbase_id), None).await;

    let replay = continue_replay(&mut state, &current_branch_name, landing_display, false).await?;

    Ok(RebaseOutput {
        action: "start".to_string(),
        status: "completed".to_string(),
        branch: current_branch_name,
        commit: state.current_head.to_string(),
        upstream: Some(upstream.to_string()),
        onto: Some(newbase_id.to_string()),
        common_ancestor: Some(base_id.to_string()),
        replay_count: Some(replay_count),
        previous_commit: Some(head_to_rebase_id.to_string()),
        restored: None,
        applied_commits: replay.applied_commits,
        dropped_commits: replay.dropped_commits,
        skipped_commit: None,
        skipped_subject: None,
        remaining: Some(state.todo.len()),
    })
}

/// Slim summary returned to `libra pull --rebase`. The full
/// [`RebaseOutput`] carries fields that only make sense for the
/// rebase subcommand (e.g. `restored`, `applied_commits`,
/// `skipped_subject`); pull only needs to render the integration
/// outcome alongside its fetch summary.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PullRebaseSummary {
    /// One of `"fast-forwarded"`, `"already-up-to-date"`,
    /// `"completed"`, or `"no-commits"`.
    pub status: String,
    /// The branch that was rebased.
    pub branch: String,
    /// HEAD before the rebase.
    pub old_commit: String,
    /// HEAD after the rebase (== `old_commit` for the no-op cases).
    pub commit: String,
    /// The upstream tip the branch was rebased onto.
    pub onto: String,
    /// Number of commits replayed during the rebase. `0` for the
    /// fast-forward / already-up-to-date / no-commits branches.
    pub replay_count: usize,
}

/// Run `run_rebase_start` and project the result down to the
/// [`PullRebaseSummary`] that `libra pull --rebase` renders. Failure
/// modes (conflict, dirty worktree, etc.) propagate via
/// [`RebaseError`] which already has a `From<…> for CliError` impl
/// with structured hints — pull just wraps it in its own error
/// variant so the `phase=rebase` detail can be attached.
pub(crate) async fn run_rebase_for_pull(upstream: &str) -> Result<PullRebaseSummary, RebaseError> {
    // `pull --rebase` keeps Libra's default (keep become-empty commits).
    let output = run_rebase_start(
        upstream,
        None,
        false,
        false,
        false,
        RebaseEmptyMode::Keep,
        false,
    )
    .await?;
    let old_commit = output
        .previous_commit
        .clone()
        .unwrap_or_else(|| output.commit.clone());
    Ok(PullRebaseSummary {
        status: output.status,
        branch: output.branch,
        old_commit,
        commit: output.commit,
        onto: output.onto.unwrap_or_else(|| upstream.to_string()),
        replay_count: output.replay_count.unwrap_or(0),
    })
}

/// Continue replaying commits from the current state
async fn continue_replay(
    state: &mut RebaseState,
    branch_name: &str,
    upstream_display: &str,
    emit_human: bool,
) -> Result<RebaseReplaySummary, RebaseError> {
    let db = get_db_conn_instance().await;
    let mut summary = RebaseReplaySummary::default();

    if emit_human {
        println!(
            "Rebasing {} commits from `{}` onto `{}`...",
            state.todo.len(),
            branch_name,
            upstream_display
        );
    }

    while let Some(commit_id) = state.todo.front().cloned() {
        let action = state
            .todo_actions
            .front()
            .copied()
            .unwrap_or(RebaseTodoAction::Pick);
        match replay_commit_with_conflict_detection(
            &commit_id,
            &state.current_head,
            action,
            state.empty_mode,
        )
        .await
        {
            ReplayResult::BecameEmptyDropped { subject } => {
                // `--empty=drop`: the commit became empty after replay; skip it
                // without advancing `current_head` (the new parent is unchanged).
                state.todo.pop_front();
                state.todo_actions.pop_front();
                state.stopped_sha = None;
                record_rebase_rewrite(commit_id, state.current_head, state.current_head, false)?;
                if emit_human {
                    println!(
                        "dropping {} {} -- patch contents already upstream",
                        commit_id, subject
                    );
                }
                summary.dropped_commits.push(RebaseDroppedCommitOutput {
                    commit: commit_id.to_string(),
                    subject,
                });
                if let Err(e) = state.save().await {
                    if emit_human {
                        emit_warning(format!("failed to save rebase state: {}", e));
                    } else {
                        return Err(RebaseError::StateSave(e));
                    }
                }
            }
            ReplayResult::Success(replayed_commit_id) => {
                let subject = commit_subject_lossy(&commit_id, emit_human);
                let previous_tip = state.current_head;
                state.current_head = replayed_commit_id;
                // Move commit from todo to done
                state.todo.pop_front();
                state.todo_actions.pop_front();
                state.done.push(commit_id);
                state.stopped_sha = None;

                // Update HEAD
                Head::update_with_conn(&db, Head::Detached(state.current_head), None).await;

                if emit_human {
                    println!(
                        "Applied: {} {}",
                        short_object_id(&state.current_head),
                        subject
                    );
                }
                summary.applied_commits.push(RebaseAppliedCommitOutput {
                    original_commit: commit_id.to_string(),
                    commit: state.current_head.to_string(),
                    subject,
                });

                // Save state after each successful commit
                if let Err(e) = state.save().await {
                    if emit_human {
                        emit_warning(format!("failed to save rebase state: {}", e));
                    } else {
                        return Err(RebaseError::StateSave(e));
                    }
                }
                record_rebase_rewrite(
                    commit_id,
                    previous_tip,
                    replayed_commit_id,
                    action.folds_into_previous(),
                )?;
                schedule_rebase_exec(state).await?;
                if let Some(applied) = summary.applied_commits.last_mut() {
                    applied.commit = state.current_head.to_string();
                }
            }
            ReplayResult::Conflict { paths, message } => {
                let subject = commit_subject_lossy(&commit_id, emit_human);
                // Save state with stopped_sha
                state.stopped_sha = Some(commit_id);
                if let Err(e) = state.save().await {
                    return Err(RebaseError::StateSave(e));
                }

                if emit_human {
                    eprintln!(
                        "error: could not apply {}: {}",
                        short_object_id(&commit_id),
                        subject
                    );
                    if let Some(message) = message.as_ref() {
                        eprintln!("fatal: {}", message);
                    }

                    eprintln!("CONFLICT in {} file(s):", paths.len());
                    for path in &paths {
                        eprintln!("  {}", path.display());
                    }
                    eprintln!();
                    eprintln!("After resolving conflicts, mark them with 'libra add <file>'");
                    eprintln!("then run 'libra rebase --continue'");
                    eprintln!("To skip this commit, run 'libra rebase --skip'");
                    eprintln!(
                        "To abort and return to the original branch, run 'libra rebase --abort'"
                    );
                }
                return Err(RebaseError::ReplayConflict {
                    commit: commit_id.to_string(),
                    subject,
                    paths,
                    message,
                });
            }
            ReplayResult::Internal { kind, detail } => {
                let subject = commit_subject_lossy(&commit_id, emit_human);
                state.stopped_sha = Some(commit_id);
                if let Err(e) = state.save().await {
                    return Err(RebaseError::StateSave(e));
                }

                if emit_human {
                    eprintln!(
                        "error: could not apply {}: {}",
                        short_object_id(&commit_id),
                        subject
                    );
                    eprintln!("fatal: {}: {}", kind.as_str(), detail);
                    eprintln!(
                        "To abort and return to the original branch, run 'libra rebase --abort'"
                    );
                }
                return Err(RebaseError::ReplayInternal {
                    commit: commit_id.to_string(),
                    subject,
                    kind,
                    detail,
                });
            }
        }
    }

    // All commits replayed successfully - finalize
    finalize_rebase(state, emit_human)
        .await
        .map_err(|e| RebaseError::Finalize(e.to_string()))?;
    Ok(summary)
}

/// Finalize rebase after all commits are replayed
async fn finalize_rebase(state: &RebaseState, emit_human: bool) -> anyhow::Result<()> {
    let db = get_db_conn_instance().await;
    let final_commit_id = state.current_head;
    let aux = RebaseAuxState::load_optional().context("failed to load rebase auxiliary state")?;
    let mut ref_updates = Vec::new();
    if let Some(aux) = aux.as_ref()
        && aux.update_refs
    {
        for update in &aux.refs_to_update {
            let old_oid = ObjectHash::from_str(&update.old_oid)
                .map_err(anyhow::Error::msg)
                .with_context(|| {
                    format!(
                        "rebase update-refs recorded an invalid old object for branch '{}'",
                        update.branch
                    )
                })?;
            let new_oid = resolve_rebase_rewrite(aux, &update.old_oid, state.onto)
                .with_context(|| format!("failed to retarget branch '{}'", update.branch))?;
            ref_updates.push((update.branch.clone(), old_oid, new_oid));
        }
    }

    // Prepare the index/worktree before moving any refs. If materialization
    // fails, the branch tips remain untouched and `--continue` can retry.
    let final_commit: Commit =
        load_object(&state.current_head).context("failed to load final commit for rebase")?;
    let final_tree: Tree =
        load_object(&final_commit.tree_id).context("failed to load final tree for rebase")?;

    let index_file = path::index();
    let current_index = git_internal::internal::index::Index::load(&index_file)
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to load current index before rebase finish")?;
    let mut index = git_internal::internal::index::Index::new();
    rebuild_index_from_tree(&final_tree, &mut index, "")
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to rebuild index from final tree")?;
    reset_workdir_tracked_only(&current_index, &index)
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to reset working directory after rebase")?;
    index
        .save(&index_file)
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to save index after rebase")?;

    let finish_action = ReflogAction::Rebase {
        state: "finish".to_string(),
        details: format!("returning to refs/heads/{}", state.head_name),
    };
    let finish_context = ReflogContext {
        old_oid: state.orig_head.to_string(),
        new_oid: final_commit_id.to_string(),
        action: finish_action,
    };

    let branch_name_cloned = state.head_name.clone();
    let expected_branch_tip = state.orig_head;
    if let Err(e) = with_reflog(
        finish_context,
        move |txn: &sea_orm::DatabaseTransaction| {
            let ref_updates = ref_updates.clone();
            Box::pin(async move {
                let live_branch = Branch::find_branch_result_with_conn(
                    txn,
                    &branch_name_cloned,
                    None,
                )
                .await
                .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?
                .ok_or_else(|| {
                    sea_orm::DbErr::Custom(format!(
                        "rebased branch '{}' disappeared before finalization",
                        branch_name_cloned
                    ))
                })?;
                if live_branch.commit != expected_branch_tip
                    && live_branch.commit != final_commit_id
                {
                    return Err(sea_orm::DbErr::Custom(format!(
                        "rebased branch '{}' moved from {} to {} while the rebase was running",
                        branch_name_cloned, expected_branch_tip, live_branch.commit
                    )));
                }

                for (branch, old_oid, new_oid) in ref_updates {
                    let live = Branch::find_branch_result_with_conn(txn, &branch, None)
                        .await
                        .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?
                        .ok_or_else(|| {
                            sea_orm::DbErr::Custom(format!(
                                "branch '{branch}' disappeared during rebase --update-refs"
                            ))
                        })?;
                    if live.commit == new_oid {
                        continue;
                    }
                    if live.commit != old_oid {
                        return Err(sea_orm::DbErr::Custom(format!(
                            "branch '{branch}' moved from {old_oid} to {} during rebase --update-refs",
                            live.commit
                        )));
                    }
                    Branch::update_branch_with_conn(
                        txn,
                        &branch,
                        &new_oid.to_string(),
                        None,
                    )
                    .await?;
                    let context = ReflogContext {
                        old_oid: old_oid.to_string(),
                        new_oid: new_oid.to_string(),
                        action: ReflogAction::Rebase {
                            state: "update-refs".to_string(),
                            details: format!("updating refs/heads/{branch}"),
                        },
                    };
                    reflog::Reflog::insert_single_entry(
                        txn,
                        &context,
                        &format!("refs/heads/{branch}"),
                    )
                    .await
                    .map_err(|error| {
                        sea_orm::DbErr::Custom(format!(
                            "failed to record update-refs reflog for '{branch}': {error}"
                        ))
                    })?;
                }

                // This is the crucial step: move the original branch from its old position
                // to the final replayed commit.
                if live_branch.commit != final_commit_id {
                    Branch::update_branch_with_conn(
                        txn,
                        &branch_name_cloned,
                        &final_commit_id.to_string(),
                        None,
                    )
                    .await?;
                }

                // Also, re-attach HEAD to the newly moved branch.
                Head::update_with_conn(txn, Head::Branch(branch_name_cloned.clone()), None).await;
                Ok(())
            })
        },
        true,
    )
    .await
    {
        // Attempt to restore HEAD to a safe state
        Head::update_with_conn(&db, Head::Detached(final_commit_id), None).await;
        return Err(e).context("failed to record reflog for rebase finish");
    }

    RebaseState::cleanup()
        .await
        .map_err(anyhow::Error::msg)
        .context("failed to clean up completed rebase state")?;
    resolve_rebase_autostash()
        .await
        .context("failed to restore rebase autostash")?;
    RebaseAuxState::cleanup().context("failed to clean up rebase auxiliary state")?;

    if emit_human {
        println!(
            "Successfully rebased branch '{}' onto '{}'.",
            state.head_name,
            short_object_id(&state.onto)
        );
    }
    Ok(())
}

async fn run_rebase_continue() -> Result<RebaseOutput, RebaseError> {
    ensure_rebase_in_progress().await?;
    let mut state = RebaseState::load().await.map_err(RebaseError::StateLoad)?;
    let previous_commit = state.current_head.to_string();
    let branch = state.head_name.clone();
    let onto_display = short_object_id(&state.onto);
    let mut applied_commits = Vec::new();
    let mut dropped_commits = Vec::new();

    if RebaseAuxState::load_optional()?
        .and_then(|aux| aux.pending_exec)
        .is_some()
    {
        run_pending_rebase_exec(&mut state).await?;
        if state.todo.is_empty() {
            finalize_rebase(&state, false)
                .await
                .map_err(|error| RebaseError::Finalize(error.to_string()))?;
        } else {
            let replay = continue_replay(&mut state, &branch, &onto_display, false).await?;
            applied_commits.extend(replay.applied_commits);
            dropped_commits.extend(replay.dropped_commits);
        }
        return Ok(RebaseOutput {
            action: "continue".to_string(),
            status: "completed".to_string(),
            branch,
            commit: state.current_head.to_string(),
            upstream: None,
            onto: Some(state.onto.to_string()),
            common_ancestor: None,
            replay_count: None,
            previous_commit: Some(previous_commit),
            restored: None,
            applied_commits,
            dropped_commits,
            skipped_commit: None,
            skipped_subject: None,
            remaining: Some(state.todo.len()),
        });
    }

    if let Some(stopped_sha) = state.stopped_sha {
        // Create a commit from the current index after the user has resolved
        // conflicts and staged the resolution.
        let index_file = path::index();
        let index = git_internal::internal::index::Index::load(&index_file)
            .map_err(|e| RebaseError::IndexLoad(e.to_string()))?;

        if has_unmerged_entries(&index) {
            return Err(RebaseError::UnresolvedConflicts);
        }

        // rerere: the conflict is resolved — record its postimage so an identical
        // conflict is auto-resolved next time. A no-op unless `rerere.enabled`.
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update on rebase --continue failed: {error}");
        }

        let new_tree_id =
            create_tree_from_index(&index).map_err(|e| RebaseError::TreeCreate(e.to_string()))?;

        let original_commit: Commit =
            load_object(&stopped_sha).map_err(|e| RebaseError::CommitLoad {
                commit: stopped_sha.to_string(),
                detail: e.to_string(),
            })?;
        let subject = commit_subject_from_message(&original_commit.message);

        let action = state
            .todo_actions
            .front()
            .copied()
            .unwrap_or(RebaseTodoAction::Pick);
        let new_commit =
            create_replayed_commit(&original_commit, new_tree_id, state.current_head, action)
                .map_err(|detail| RebaseError::CommitLoad {
                    commit: state.current_head.to_string(),
                    detail,
                })?;
        save_object(&new_commit, &new_commit.id)
            .map_err(|e| RebaseError::CommitSave(e.to_string()))?;

        let previous_tip = state.current_head;
        state.current_head = new_commit.id;
        state.todo.pop_front();
        state.todo_actions.pop_front();
        state.done.push(stopped_sha);
        state.stopped_sha = None;

        let db = get_db_conn_instance().await;
        Head::update_with_conn(&db, Head::Detached(state.current_head), None).await;

        applied_commits.push(RebaseAppliedCommitOutput {
            original_commit: stopped_sha.to_string(),
            commit: state.current_head.to_string(),
            subject,
        });
        state.save().await.map_err(RebaseError::StateSave)?;
        record_rebase_rewrite(
            stopped_sha,
            previous_tip,
            state.current_head,
            action.folds_into_previous(),
        )?;
        schedule_rebase_exec(&mut state).await?;
        if let Some(applied) = applied_commits.last_mut() {
            applied.commit = state.current_head.to_string();
        }
    }

    if state.todo.is_empty() {
        finalize_rebase(&state, false)
            .await
            .map_err(|e| RebaseError::Finalize(e.to_string()))?;
    } else {
        state.save().await.map_err(RebaseError::StateSave)?;
        let replay = continue_replay(&mut state, &branch, &onto_display, false).await?;
        applied_commits.extend(replay.applied_commits);
        dropped_commits.extend(replay.dropped_commits);
    }

    Ok(RebaseOutput {
        action: "continue".to_string(),
        status: "completed".to_string(),
        branch,
        commit: state.current_head.to_string(),
        upstream: None,
        onto: Some(state.onto.to_string()),
        common_ancestor: None,
        replay_count: None,
        previous_commit: Some(previous_commit),
        restored: None,
        applied_commits,
        dropped_commits,
        skipped_commit: None,
        skipped_subject: None,
        remaining: Some(state.todo.len()),
    })
}

async fn run_rebase_abort() -> Result<RebaseOutput, RebaseError> {
    match RebaseState::is_in_progress().await {
        Ok(true) => {}
        Ok(false) => return Err(RebaseError::NoRebaseInProgress),
        Err(e) => return Err(RebaseError::StateCheck(e)),
    }

    let state = RebaseState::load().await.map_err(RebaseError::StateLoad)?;
    let orig_head = state.orig_head;
    let orig_head_str = orig_head.to_string();

    // Restore files and index before changing HEAD. A materialization failure
    // leaves the branch/ref state untouched and the abort can be retried.
    let orig_commit: Commit =
        load_object(&orig_head).map_err(|error| RebaseError::OriginalCommitLoad {
            commit: orig_head.to_string(),
            detail: error.to_string(),
        })?;
    let orig_tree: Tree =
        load_object(&orig_commit.tree_id).map_err(|error| RebaseError::OriginalTreeLoad {
            tree: orig_commit.tree_id.to_string(),
            detail: error.to_string(),
        })?;
    let index_file = path::index();
    let current_index = git_internal::internal::index::Index::load(&index_file)
        .map_err(|error| RebaseError::IndexLoad(error.to_string()))?;
    let mut index = git_internal::internal::index::Index::new();
    rebuild_index_from_tree(&orig_tree, &mut index, "").map_err(RebaseError::IndexRebuild)?;
    reset_workdir_tracked_only(&current_index, &index).map_err(RebaseError::WorkdirReset)?;
    index
        .save(&index_file)
        .map_err(|error| RebaseError::IndexSave(error.to_string()))?;

    // Restore HEAD to original branch
    let abort_action = ReflogAction::Rebase {
        state: "abort".to_string(),
        details: format!("returning to refs/heads/{}", state.head_name),
    };
    let abort_context = ReflogContext {
        old_oid: state.current_head.to_string(),
        new_oid: orig_head_str.clone(),
        action: abort_action,
    };

    let branch_name_cloned = state.head_name.clone();
    let replay_tip = state.current_head;
    with_reflog(
        abort_context,
        move |txn: &sea_orm::DatabaseTransaction| {
            Box::pin(async move {
                let live = Branch::find_branch_result_with_conn(txn, &branch_name_cloned, None)
                    .await
                    .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?
                    .ok_or_else(|| {
                        sea_orm::DbErr::Custom(format!(
                            "branch '{}' disappeared during rebase abort",
                            branch_name_cloned
                        ))
                    })?;
                if live.commit != orig_head && live.commit != replay_tip {
                    return Err(sea_orm::DbErr::Custom(format!(
                        "branch '{}' moved from {} to {} while the rebase was running",
                        branch_name_cloned, orig_head, live.commit
                    )));
                }
                if live.commit == replay_tip && replay_tip != orig_head {
                    Branch::update_branch_with_conn(
                        txn,
                        &branch_name_cloned,
                        &orig_head.to_string(),
                        None,
                    )
                    .await?;
                    let context = ReflogContext {
                        old_oid: replay_tip.to_string(),
                        new_oid: orig_head.to_string(),
                        action: ReflogAction::Rebase {
                            state: "abort".to_string(),
                            details: format!("returning to refs/heads/{branch_name_cloned}"),
                        },
                    };
                    reflog::Reflog::insert_single_entry(
                        txn,
                        &context,
                        &format!("refs/heads/{branch_name_cloned}"),
                    )
                    .await
                    .map_err(|error| {
                        sea_orm::DbErr::Custom(format!(
                            "failed to record abort reflog for '{}': {error}",
                            branch_name_cloned
                        ))
                    })?;
                }
                Head::update_with_conn(txn, Head::Branch(branch_name_cloned), None).await;
                Ok(())
            })
        },
        false,
    )
    .await
    .map_err(|error| RebaseError::BranchRestore {
        branch: state.head_name.clone(),
        detail: error.to_string(),
    })?;

    RebaseState::cleanup()
        .await
        .map_err(RebaseError::StateSave)?;
    resolve_rebase_autostash().await?;
    RebaseAuxState::cleanup()?;

    Ok(RebaseOutput {
        action: "abort".to_string(),
        status: "aborted".to_string(),
        branch: state.head_name,
        commit: orig_head_str,
        upstream: None,
        onto: None,
        common_ancestor: None,
        replay_count: None,
        previous_commit: Some(state.current_head.to_string()),
        restored: Some(true),
        applied_commits: Vec::new(),
        dropped_commits: Vec::new(),
        skipped_commit: None,
        skipped_subject: None,
        remaining: None,
    })
}

async fn run_rebase_skip() -> Result<RebaseOutput, RebaseError> {
    ensure_rebase_in_progress().await?;
    let mut state = RebaseState::load().await.map_err(RebaseError::StateLoad)?;
    let previous_commit = state.current_head.to_string();
    let branch = state.head_name.clone();
    let onto_display = short_object_id(&state.onto);

    if let Some(mut aux) = RebaseAuxState::load_optional()?
        && aux.pending_exec.is_some()
    {
        reconcile_rebase_exec_head(&mut state, &mut aux).await?;
        let quiet_output = OutputConfig {
            quiet: true,
            ..Default::default()
        };
        switch::ensure_clean_status(&quiet_output)
            .await
            .map_err(|error| RebaseError::WorktreeDirty {
                action: "skip the failed exec command".to_string(),
                detail: error.to_string(),
            })?;
        aux.pending_exec = None;
        aux.save()?;
        let mut applied_commits = Vec::new();
        let mut dropped_commits = Vec::new();
        if state.todo.is_empty() {
            finalize_rebase(&state, false)
                .await
                .map_err(|error| RebaseError::Finalize(error.to_string()))?;
        } else {
            let replay = continue_replay(&mut state, &branch, &onto_display, false).await?;
            applied_commits.extend(replay.applied_commits);
            dropped_commits.extend(replay.dropped_commits);
        }
        return Ok(RebaseOutput {
            action: "skip".to_string(),
            status: "completed".to_string(),
            branch,
            commit: state.current_head.to_string(),
            upstream: None,
            onto: Some(state.onto.to_string()),
            common_ancestor: None,
            replay_count: None,
            previous_commit: Some(previous_commit),
            restored: None,
            applied_commits,
            dropped_commits,
            skipped_commit: None,
            skipped_subject: None,
            remaining: Some(state.todo.len()),
        });
    }

    let skipped_sha = state
        .stopped_sha
        .or_else(|| state.todo.front().cloned())
        .ok_or(RebaseError::NoCommitToSkip)?;
    let skipped_subject = match load_object::<Commit>(&skipped_sha) {
        Ok(commit) => Some(commit_subject_from_message(&commit.message)),
        Err(_) => None,
    };
    record_rebase_rewrite(skipped_sha, state.current_head, state.current_head, false)?;

    state.todo.pop_front();
    let skipped_action = state.todo_actions.pop_front();
    state.stopped_sha = None;
    if skipped_action.unwrap_or(RebaseTodoAction::Pick) == RebaseTodoAction::Pick {
        downgrade_leading_autosquash_dependents(&mut state.todo_actions);
    }

    let current_commit: Commit =
        load_object(&state.current_head).map_err(|e| RebaseError::CommitLoad {
            commit: state.current_head.to_string(),
            detail: e.to_string(),
        })?;
    let current_tree: Tree =
        load_object(&current_commit.tree_id).map_err(|e| RebaseError::OriginalTreeLoad {
            tree: current_commit.tree_id.to_string(),
            detail: e.to_string(),
        })?;

    let index_file = path::index();
    let current_index = git_internal::internal::index::Index::load(&index_file)
        .map_err(|e| RebaseError::IndexLoad(e.to_string()))?;
    let mut index = git_internal::internal::index::Index::new();
    rebuild_index_from_tree(&current_tree, &mut index, "")
        .map_err(|e| RebaseError::IndexRebuild(e.to_string()))?;
    index
        .save(&index_file)
        .map_err(|e| RebaseError::IndexSave(e.to_string()))?;
    reset_workdir_tracked_only(&current_index, &index)
        .map_err(|e| RebaseError::WorkdirReset(e.to_string()))?;

    let mut applied_commits = Vec::new();
    let mut dropped_commits = Vec::new();
    if state.todo.is_empty() {
        finalize_rebase(&state, false)
            .await
            .map_err(|e| RebaseError::Finalize(e.to_string()))?;
    } else {
        state.save().await.map_err(RebaseError::StateSave)?;
        let replay = continue_replay(&mut state, &branch, &onto_display, false).await?;
        applied_commits.extend(replay.applied_commits);
        dropped_commits.extend(replay.dropped_commits);
    }

    Ok(RebaseOutput {
        action: "skip".to_string(),
        status: "completed".to_string(),
        branch,
        commit: state.current_head.to_string(),
        upstream: None,
        onto: Some(state.onto.to_string()),
        common_ancestor: None,
        replay_count: None,
        previous_commit: Some(previous_commit),
        restored: None,
        applied_commits,
        dropped_commits,
        skipped_commit: Some(skipped_sha.to_string()),
        skipped_subject,
        remaining: Some(state.todo.len()),
    })
}

fn downgrade_leading_autosquash_dependents(todo_actions: &mut VecDeque<RebaseTodoAction>) {
    for action in todo_actions.iter_mut() {
        if action.folds_into_previous() {
            *action = RebaseTodoAction::Pick;
        } else {
            break;
        }
    }
}

/// Check if index has unmerged entries (conflict markers)
///
/// A file is considered unmerged if it has any stage 1, 2, or 3 entry but NO stage 0 entry.
/// If a file has been staged at stage 0 (via `add`), it's considered resolved
/// even if older conflict stage entries (stages 1–3) still exist in the index.
fn has_unmerged_entries(index: &git_internal::internal::index::Index) -> bool {
    let resolved: HashSet<String> = index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| entry.name.clone())
        .collect();

    for stage in 1..=3 {
        for entry in index.tracked_entries(stage) {
            if !resolved.contains(&entry.name) {
                return true;
            }
        }
    }
    false
}

/// Create a tree from the current index
fn create_tree_from_index(
    index: &git_internal::internal::index::Index,
) -> Result<ObjectHash, String> {
    let mut items: HashMap<PathBuf, RebaseTreeEntry> = HashMap::new();
    for path in index.tracked_files() {
        let path_str = path_to_index_key(&path)?;
        if let Some(entry) = index.get(path_str, 0) {
            items.insert(
                path.clone(),
                RebaseTreeEntry {
                    hash: entry.hash,
                    mode: index_mode_to_tree_item_mode(entry.mode)?,
                },
            );
        }
    }
    create_tree_from_items_map(&items)
}

fn write_workdir_file(workdir: &Path, path: &Path, content: &[u8]) -> Result<(), String> {
    let file_path = workdir.join(path);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {}", parent.display(), e))?;
    }
    if let Ok(metadata) = fs::symlink_metadata(&file_path)
        && metadata.file_type().is_symlink()
    {
        fs::remove_file(&file_path)
            .map_err(|e| format!("failed to replace symlink {}: {}", file_path.display(), e))?;
    }
    fs::write(&file_path, content)
        .map_err(|e| format!("failed to write {}: {}", file_path.display(), e))
}

fn write_rebase_workdir_entry(
    workdir: &Path,
    path: &Path,
    entry: RebaseTreeEntry,
) -> Result<(), String> {
    let blob: Blob = load_object(&entry.hash).map_err(|error| {
        format!(
            "failed to load blob {} for worktree path '{}': {error}",
            entry.hash,
            path.display()
        )
    })?;
    write_workdir_blob(workdir, path, entry.mode, &blob.data)
}

fn write_workdir_blob(
    workdir: &Path,
    path: &Path,
    mode: TreeItemMode,
    content: &[u8],
) -> Result<(), String> {
    match mode {
        TreeItemMode::Blob => write_workdir_file(workdir, path, content),
        TreeItemMode::BlobExecutable => {
            write_workdir_file(workdir, path, content)?;
            set_executable_workdir_mode(&workdir.join(path))
        }
        TreeItemMode::Link => write_workdir_symlink(workdir, path, content),
        TreeItemMode::Tree => Err(format!(
            "tree entry cannot be written as a file: {}",
            path.display()
        )),
        TreeItemMode::Commit => Err(format!(
            "gitlink entries are not supported by rebase: {}",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn set_executable_workdir_mode(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|error| {
        format!(
            "failed to set executable mode on {}: {error}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_executable_workdir_mode(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn write_workdir_symlink(workdir: &Path, path: &Path, target: &[u8]) -> Result<(), String> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let file_path = workdir.join(path);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    if fs::symlink_metadata(&file_path).is_ok() {
        fs::remove_file(&file_path)
            .map_err(|error| format!("failed to replace {}: {error}", file_path.display()))?;
    }
    std::os::unix::fs::symlink(
        PathBuf::from(OsString::from_vec(target.to_vec())),
        &file_path,
    )
    .map_err(|error| format!("failed to create symlink {}: {error}", file_path.display()))
}

#[cfg(not(unix))]
fn write_workdir_symlink(workdir: &Path, path: &Path, target: &[u8]) -> Result<(), String> {
    write_workdir_file(workdir, path, target)
}

fn write_conflict_file(workdir: &Path, path: &Path, content: &str) -> Result<(), String> {
    write_workdir_file(workdir, path, content.as_bytes())
        .map_err(|e| format!("conflict file: {}", e))
}

fn conflict_marker_eol() -> &'static str {
    if cfg!(windows) { "\r\n" } else { "\n" }
}

fn conflict_payload(content: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(content) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(format!("[binary content, {} bytes]", content.len())),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RebaseTreeEntry {
    hash: ObjectHash,
    mode: TreeItemMode,
}

fn collect_tree_items_and_paths<'a>(
    trees: impl IntoIterator<Item = &'a Tree>,
) -> (Vec<HashMap<PathBuf, RebaseTreeEntry>>, HashSet<PathBuf>) {
    let mut items = Vec::new();
    let mut all_paths = HashSet::new();
    for tree in trees {
        let map: HashMap<PathBuf, RebaseTreeEntry> = tree
            .get_plain_items_with_mode()
            .into_iter()
            .filter_map(|(path, hash, mode)| {
                if mode == TreeItemMode::Commit {
                    None
                } else {
                    Some((path, RebaseTreeEntry { hash, mode }))
                }
            })
            .collect();
        all_paths.extend(map.keys().cloned());
        items.push(map);
    }
    (items, all_paths)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
    };

    use git_internal::{
        hash::ObjectHash,
        internal::object::tree::{Tree, TreeItem, TreeItemMode},
    };
    use tempfile::tempdir;

    #[cfg(unix)]
    use super::path_to_index_key;
    use super::{
        RebaseError, RebaseTreeEntry, ReplayErrorKind, classify_relative_to_base,
        collect_tree_items_and_paths, create_tree_from_items_map, index_mode_to_tree_item_mode,
        resolve_three_way, tree_item_mode_to_index_mode, tree_item_name, write_workdir_blob,
    };
    use crate::{
        command::load_object,
        utils::{
            error::{CliError, StableErrorCode},
            test::{ChangeDirGuard, setup_with_new_libra_in},
        },
    };

    fn rebase_entry(byte: u8, mode: TreeItemMode) -> RebaseTreeEntry {
        RebaseTreeEntry {
            hash: ObjectHash::new(&[byte; 20]),
            mode,
        }
    }

    #[test]
    fn replay_error_kind_stable_codes_route_distinct_failures() {
        // Object load failures point at repository corruption.
        for kind in [
            ReplayErrorKind::CommitLoad,
            ReplayErrorKind::MissingParent,
            ReplayErrorKind::BaseTreeLoad,
            ReplayErrorKind::TheirTreeLoad,
            ReplayErrorKind::OurTreeLoad,
            ReplayErrorKind::NewTreeLoad,
        ] {
            assert_eq!(
                kind.stable_code(),
                StableErrorCode::RepoCorrupt,
                "{kind:?} should map to RepoCorrupt"
            );
        }

        // Pure index read maps to IO read.
        assert_eq!(
            ReplayErrorKind::IndexLoad.stable_code(),
            StableErrorCode::IoReadFailed
        );

        // Untracked file collision is a blocked operation, not an unresolved conflict.
        assert_eq!(
            ReplayErrorKind::UntrackedOverwrite.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );

        // Write/save side failures all surface as IO write failed.
        for kind in [
            ReplayErrorKind::ConflictMarker,
            ReplayErrorKind::TreeCreate,
            ReplayErrorKind::CommitSave,
            ReplayErrorKind::IndexRebuild,
            ReplayErrorKind::IndexSave,
            ReplayErrorKind::WorkdirReset,
        ] {
            assert_eq!(
                kind.stable_code(),
                StableErrorCode::IoWriteFailed,
                "{kind:?} should map to IoWriteFailed"
            );
        }
    }

    #[test]
    fn replay_error_kind_serializes_snake_case_identifiers() {
        assert_eq!(ReplayErrorKind::IndexLoad.as_str(), "index_load");
        assert_eq!(ReplayErrorKind::CommitLoad.as_str(), "commit_load");
        assert_eq!(ReplayErrorKind::MissingParent.as_str(), "missing_parent");
        assert_eq!(ReplayErrorKind::BaseTreeLoad.as_str(), "base_tree_load");
        assert_eq!(ReplayErrorKind::TheirTreeLoad.as_str(), "their_tree_load");
        assert_eq!(ReplayErrorKind::OurTreeLoad.as_str(), "our_tree_load");
        assert_eq!(
            ReplayErrorKind::UntrackedOverwrite.as_str(),
            "untracked_overwrite"
        );
        assert_eq!(ReplayErrorKind::ConflictMarker.as_str(), "conflict_marker");
        assert_eq!(ReplayErrorKind::TreeCreate.as_str(), "tree_create");
        assert_eq!(ReplayErrorKind::CommitSave.as_str(), "commit_save");
        assert_eq!(ReplayErrorKind::NewTreeLoad.as_str(), "new_tree_load");
        assert_eq!(ReplayErrorKind::IndexRebuild.as_str(), "index_rebuild");
        assert_eq!(ReplayErrorKind::IndexSave.as_str(), "index_save");
        assert_eq!(ReplayErrorKind::WorkdirReset.as_str(), "workdir_reset");
    }

    /// Pin the `Display` format for the static-message `RebaseError`
    /// variants. These strings are used directly as the `CliError`
    /// message via `CliError::fatal(error.to_string())` in the
    /// `From<RebaseError> for CliError` mapping, so they're part of
    /// the human + JSON output contract.
    ///
    /// Source-chained variants (CheckStateLoad, LoadStateError,
    /// UpstreamLookup, WorktreeStatus, etc.) are intentionally not
    /// pinned here — their `{0}` slot forwards to upstream Display
    /// strings owned by other modules.
    #[test]
    fn rebase_error_display_pins_static_message_variants() {
        assert_eq!(
            RebaseError::NoRebaseInProgress.to_string(),
            "no rebase in progress",
        );
        assert_eq!(
            RebaseError::NotOnBranch.to_string(),
            "not on a branch or in detached HEAD state, cannot rebase",
        );
        assert_eq!(
            RebaseError::NoCommonAncestor.to_string(),
            "no common ancestor found",
        );
        assert_eq!(
            RebaseError::UnresolvedConflicts.to_string(),
            "you must resolve all conflicts before continuing",
        );
        assert_eq!(RebaseError::NoCommitToSkip.to_string(), "no commit to skip");
        assert_eq!(
            RebaseError::BranchHasNoCommits {
                branch: "main".to_string(),
            }
            .to_string(),
            "current branch 'main' has no commits",
        );
        assert_eq!(
            RebaseError::UntrackedOverwrite {
                path: "scratch.txt".to_string(),
            }
            .to_string(),
            "untracked working tree file would be overwritten by rebase: scratch.txt",
        );
        assert_eq!(
            RebaseError::UpstreamResolve {
                upstream: "origin/main".to_string(),
                detail: "not a valid object".to_string(),
            }
            .to_string(),
            "failed to resolve upstream 'origin/main': not a valid object",
        );
        assert_eq!(
            RebaseError::WorktreeDirty {
                action: "switch".to_string(),
                detail: "uncommitted changes".to_string(),
            }
            .to_string(),
            "uncommitted changes, can't switch",
        );
    }

    /// Pin the `From<RebaseError> for CliError` stable_code mapping
    /// for every RebaseError variant. RebaseError itself has no
    /// `stable_code()` method — the routing lives in the `From`
    /// impl at `:623-722`, so this is the only place where the
    /// wire surface ("which StableErrorCode does each variant
    /// produce in --json envelopes?") can be locked down.
    ///
    /// The 25 variants collapse into 6 stable codes via a match
    /// with many alternations. A future refactor that re-routed
    /// any variant — e.g. flipping `OriginalTreeLoad` from
    /// `RepoCorrupt` to `IoReadFailed`, or accidentally landing
    /// `IndexLoad` in the IoWriteFailed group with its siblings —
    /// would silently change client retry classification unless
    /// every variant has its own guard.
    ///
    /// `ReplayInternal` delegates to `ReplayErrorKind::stable_code()`
    /// which has its own enumeration in
    /// `replay_error_kind_stable_codes_route_distinct_failures`; we
    /// pin one representative kind (`CommitSave`) here to lock the
    /// delegation itself.
    ///
    /// Continuation of the v0.17.701..v0.17.708 surface-contract
    /// sweep (TuiControlError / CherryPickError / RevertError /
    /// RestoreError / StashError / ResetError / FuseUmountError /
    /// WorktreeError). Per the prioritised backlog, rebase.rs was
    /// the last HIGH-priority pin gap.
    #[test]
    fn rebase_error_stable_code_pins_each_variant() {
        fn code_of(err: RebaseError) -> StableErrorCode {
            CliError::from(err).stable_code()
        }

        assert_eq!(
            code_of(RebaseError::NoRebaseInProgress),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::StateCheck("ignored".to_string())),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            code_of(RebaseError::StateLoad("ignored".to_string())),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            code_of(RebaseError::NotOnBranch),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::BranchHasNoCommits {
                branch: "ignored".to_string(),
            }),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::UpstreamResolve {
                upstream: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            code_of(RebaseError::NoCommonAncestor),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            code_of(RebaseError::WorktreeStatus("ignored".to_string())),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            code_of(RebaseError::WorktreeDirty {
                action: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::UntrackedOverwrite {
                path: "ignored".to_string(),
            }),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            code_of(RebaseError::UnresolvedConflicts),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            code_of(RebaseError::NoCommitToSkip),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::ReplayConflict {
                commit: "ignored".to_string(),
                subject: "ignored".to_string(),
                paths: Vec::new(),
                message: None,
            }),
            StableErrorCode::ConflictUnresolved,
        );
        // ReplayInternal delegates to ReplayErrorKind::stable_code();
        // exhaustive ReplayErrorKind routing is pinned by
        // replay_error_kind_stable_codes_route_distinct_failures.
        assert_eq!(
            code_of(RebaseError::ReplayInternal {
                commit: "ignored".to_string(),
                subject: "ignored".to_string(),
                kind: ReplayErrorKind::CommitSave,
                detail: "ignored".to_string(),
            }),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::BranchRestore {
                branch: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::CommitLoad {
                commit: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            code_of(RebaseError::OriginalCommitLoad {
                commit: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            code_of(RebaseError::OriginalTreeLoad {
                tree: "ignored".to_string(),
                detail: "ignored".to_string(),
            }),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            code_of(RebaseError::IndexLoad("ignored".to_string())),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            code_of(RebaseError::TreeCreate("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::CommitSave("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::IndexRebuild("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::IndexSave("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::WorkdirReset("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::StateSave("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            code_of(RebaseError::Finalize("ignored".to_string())),
            StableErrorCode::IoWriteFailed,
        );
    }

    #[test]
    fn replay_internal_error_maps_to_typed_cli_error() {
        let rebase_err = RebaseError::ReplayInternal {
            commit: "deadbeef".to_string(),
            subject: "refactor: split error kinds".to_string(),
            kind: ReplayErrorKind::CommitSave,
            detail: "disk full".to_string(),
        };
        let cli_err: CliError = rebase_err.into();
        let json: serde_json::Value = serde_json::from_str(&cli_err.render_json())
            .expect("CliError JSON payload should parse");

        assert_eq!(
            json.get("error_code").and_then(|v| v.as_str()),
            Some("LBR-IO-002")
        );
        assert_eq!(
            json.pointer("/details/kind").and_then(|v| v.as_str()),
            Some("commit_save")
        );
        assert_eq!(
            json.pointer("/details/commit").and_then(|v| v.as_str()),
            Some("deadbeef")
        );
        assert_eq!(
            json.pointer("/details/detail").and_then(|v| v.as_str()),
            Some("disk full")
        );
    }

    #[test]
    fn replay_internal_repo_corrupt_kind_keeps_separate_code() {
        let rebase_err = RebaseError::ReplayInternal {
            commit: "feedface".to_string(),
            subject: "feat: add provider".to_string(),
            kind: ReplayErrorKind::BaseTreeLoad,
            detail: "object 1234 not found".to_string(),
        };
        let cli_err: CliError = rebase_err.into();
        let json: serde_json::Value = serde_json::from_str(&cli_err.render_json())
            .expect("CliError JSON payload should parse");

        // Was previously LBR-CONFLICT-001; now distinct from real merge conflicts.
        assert_eq!(
            json.get("error_code").and_then(|v| v.as_str()),
            Some("LBR-REPO-002")
        );
        assert_eq!(
            json.pointer("/details/kind").and_then(|v| v.as_str()),
            Some("base_tree_load")
        );
    }

    #[test]
    fn tree_item_name_rejects_paths_without_file_name() {
        let err = tree_item_name(Path::new("")).expect_err("empty path should fail");
        assert!(err.contains("path has no file name"));
    }

    #[cfg(unix)]
    #[test]
    fn tree_item_name_rejects_non_utf8_paths() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let path = PathBuf::from(OsString::from_vec(vec![0x66, 0x80]));
        let err = tree_item_name(&path).expect_err("non-UTF-8 path should fail");
        assert!(err.contains("path is not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn path_to_index_key_rejects_non_utf8_paths() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let path = PathBuf::from(OsString::from_vec(vec![0x66, 0x80]));
        let err = path_to_index_key(&path).expect_err("non-UTF-8 path should fail");
        assert!(err.contains("path is not valid UTF-8"));
    }

    #[test]
    fn collect_tree_items_and_paths_unions_paths_and_preserves_items() {
        let a_hash = ObjectHash::new(&[1; 20]);
        let b_hash = ObjectHash::new(&[2; 20]);
        let b2_hash = ObjectHash::new(&[3; 20]);
        let c_hash = ObjectHash::new(&[4; 20]);

        let tree1 = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, a_hash, "a.txt".to_string()),
            TreeItem::new(TreeItemMode::BlobExecutable, b_hash, "b.txt".to_string()),
        ])
        .expect("tree1");

        let tree2 = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, b2_hash, "b.txt".to_string()),
            TreeItem::new(TreeItemMode::Link, c_hash, "c.txt".to_string()),
        ])
        .expect("tree2");

        let (items, all_paths) = collect_tree_items_and_paths([&tree1, &tree2]);
        assert_eq!(items.len(), 2);

        let expected_first: HashMap<PathBuf, RebaseTreeEntry> = HashMap::from([
            (
                PathBuf::from("a.txt"),
                RebaseTreeEntry {
                    hash: a_hash,
                    mode: TreeItemMode::Blob,
                },
            ),
            (
                PathBuf::from("b.txt"),
                RebaseTreeEntry {
                    hash: b_hash,
                    mode: TreeItemMode::BlobExecutable,
                },
            ),
        ]);
        let expected_second: HashMap<PathBuf, RebaseTreeEntry> = HashMap::from([
            (
                PathBuf::from("b.txt"),
                RebaseTreeEntry {
                    hash: b2_hash,
                    mode: TreeItemMode::Blob,
                },
            ),
            (
                PathBuf::from("c.txt"),
                RebaseTreeEntry {
                    hash: c_hash,
                    mode: TreeItemMode::Link,
                },
            ),
        ]);
        assert_eq!(items[0], expected_first);
        assert_eq!(items[1], expected_second);

        let expected_paths: HashSet<PathBuf> = HashSet::from([
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ]);
        assert_eq!(all_paths, expected_paths);
    }

    #[test]
    fn classify_relative_to_base_tracks_state() {
        let base = rebase_entry(1, TreeItemMode::Blob);
        let same = base;
        let modified = rebase_entry(2, TreeItemMode::BlobExecutable);

        match classify_relative_to_base(Some(&base), Some(&same)) {
            super::RelativeState::Same(entry) => assert_eq!(entry, base),
            other => panic!("expected Same, got {:?}", other),
        }

        match classify_relative_to_base(Some(&base), Some(&modified)) {
            super::RelativeState::Modified(entry) => assert_eq!(entry, modified),
            other => panic!("expected Modified, got {:?}", other),
        }

        match classify_relative_to_base(Some(&base), None) {
            super::RelativeState::Deleted => {}
            other => panic!("expected Deleted, got {:?}", other),
        }

        match classify_relative_to_base(None, Some(&modified)) {
            super::RelativeState::Added(entry) => assert_eq!(entry, modified),
            other => panic!("expected Added, got {:?}", other),
        }

        match classify_relative_to_base(None, None) {
            super::RelativeState::Missing => {}
            other => panic!("expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn resolve_three_way_merges_and_conflicts() {
        let base = rebase_entry(1, TreeItemMode::Blob);
        let ours = rebase_entry(2, TreeItemMode::BlobExecutable);
        let theirs = rebase_entry(3, TreeItemMode::Link);

        match resolve_three_way(Some(&base), Some(&base), Some(&base)) {
            super::MergeResolution::Use(entry) => assert_eq!(entry, base),
            other => panic!("expected Use(base), got {:?}", other),
        }

        match resolve_three_way(Some(&base), Some(&base), Some(&ours)) {
            super::MergeResolution::Use(entry) => assert_eq!(entry, ours),
            other => panic!("expected Use(ours), got {:?}", other),
        }

        match resolve_three_way(Some(&base), Some(&theirs), Some(&base)) {
            super::MergeResolution::Use(entry) => assert_eq!(entry, theirs),
            other => panic!("expected Use(theirs), got {:?}", other),
        }

        match resolve_three_way(Some(&base), Some(&theirs), Some(&ours)) {
            super::MergeResolution::Conflict(super::ConflictKind::BothChanged {
                ours: o,
                theirs: t,
            }) => {
                assert_eq!(o, ours.hash);
                assert_eq!(t, theirs.hash);
            }
            other => panic!("expected BothChanged conflict, got {:?}", other),
        }

        match resolve_three_way(None, Some(&theirs), Some(&ours)) {
            super::MergeResolution::Conflict(super::ConflictKind::BothChanged {
                ours: o,
                theirs: t,
            }) => {
                assert_eq!(o, ours.hash);
                assert_eq!(t, theirs.hash);
            }
            other => panic!("expected BothChanged conflict (add/add), got {:?}", other),
        }

        match resolve_three_way(Some(&base), None, Some(&ours)) {
            super::MergeResolution::Conflict(super::ConflictKind::OursModifiedTheirsDeleted {
                ours: o,
            }) => assert_eq!(o, ours.hash),
            other => panic!(
                "expected ours-modified/theirs-deleted conflict, got {:?}",
                other
            ),
        }

        match resolve_three_way(Some(&base), Some(&theirs), None) {
            super::MergeResolution::Conflict(super::ConflictKind::TheirsModifiedOursDeleted {
                theirs: t,
            }) => assert_eq!(t, theirs.hash),
            other => panic!(
                "expected theirs-modified/ours-deleted conflict, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn rebase_index_tree_mode_conversions_pin_supported_modes() {
        assert_eq!(
            tree_item_mode_to_index_mode(TreeItemMode::Blob).expect("regular blob"),
            0o100644
        );
        assert_eq!(
            tree_item_mode_to_index_mode(TreeItemMode::BlobExecutable).expect("executable blob"),
            0o100755
        );
        assert_eq!(
            tree_item_mode_to_index_mode(TreeItemMode::Link).expect("symlink"),
            0o120000
        );

        assert_eq!(
            index_mode_to_tree_item_mode(0o100644).expect("regular blob"),
            TreeItemMode::Blob
        );
        assert_eq!(
            index_mode_to_tree_item_mode(0o100755).expect("executable blob"),
            TreeItemMode::BlobExecutable
        );
        assert_eq!(
            index_mode_to_tree_item_mode(0o120000).expect("symlink"),
            TreeItemMode::Link
        );
        assert!(tree_item_mode_to_index_mode(TreeItemMode::Commit).is_err());
        assert!(index_mode_to_tree_item_mode(0o160000).is_err());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_tree_from_items_map_preserves_blob_modes() {
        let repo = tempdir().expect("temp repo");
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let executable = rebase_entry(1, TreeItemMode::BlobExecutable);
        let symlink = rebase_entry(2, TreeItemMode::Link);
        let regular = rebase_entry(3, TreeItemMode::Blob);
        let items = HashMap::from([
            (PathBuf::from("run.sh"), executable),
            (PathBuf::from("link"), symlink),
            (PathBuf::from("plain.txt"), regular),
        ]);

        let tree_id = create_tree_from_items_map(&items).expect("create tree");
        let tree: Tree = load_object(&tree_id).expect("load created tree");
        let modes: HashMap<_, _> = tree
            .tree_items
            .iter()
            .map(|item| (item.name.as_str(), item.mode))
            .collect();

        assert_eq!(modes.get("run.sh"), Some(&TreeItemMode::BlobExecutable));
        assert_eq!(modes.get("link"), Some(&TreeItemMode::Link));
        assert_eq!(modes.get("plain.txt"), Some(&TreeItemMode::Blob));
    }

    #[cfg(unix)]
    #[test]
    fn write_workdir_blob_replaces_existing_symlink() {
        let repo = tempdir().expect("temp repo");
        let target = repo.path().join("outside-target.txt");
        std::fs::write(&target, "outside\n").expect("write target");
        let link = repo.path().join("path.txt");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        write_workdir_blob(
            repo.path(),
            Path::new("path.txt"),
            TreeItemMode::Blob,
            b"regular\n",
        )
        .expect("write regular blob");

        assert!(
            !std::fs::symlink_metadata(&link)
                .expect("path metadata")
                .file_type()
                .is_symlink(),
            "regular blob write must replace an existing symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&link).expect("read rewritten path"),
            "regular\n"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read symlink target"),
            "outside\n",
            "regular blob write must not follow and overwrite the old symlink target"
        );
    }

    #[test]
    fn replay_error_kind_display_pins_snake_case_for_each_variant() {
        assert_eq!(ReplayErrorKind::IndexLoad.to_string(), "index_load");
        assert_eq!(ReplayErrorKind::CommitLoad.to_string(), "commit_load");
        assert_eq!(ReplayErrorKind::MissingParent.to_string(), "missing_parent");
        assert_eq!(ReplayErrorKind::BaseTreeLoad.to_string(), "base_tree_load");
        assert_eq!(
            ReplayErrorKind::TheirTreeLoad.to_string(),
            "their_tree_load",
        );
        assert_eq!(ReplayErrorKind::OurTreeLoad.to_string(), "our_tree_load");
        assert_eq!(
            ReplayErrorKind::UntrackedOverwrite.to_string(),
            "untracked_overwrite",
        );
        assert_eq!(
            ReplayErrorKind::ConflictMarker.to_string(),
            "conflict_marker",
        );
        assert_eq!(ReplayErrorKind::TreeCreate.to_string(), "tree_create");
        assert_eq!(ReplayErrorKind::CommitSave.to_string(), "commit_save");
        assert_eq!(ReplayErrorKind::NewTreeLoad.to_string(), "new_tree_load");
        assert_eq!(ReplayErrorKind::IndexRebuild.to_string(), "index_rebuild");
        assert_eq!(ReplayErrorKind::IndexSave.to_string(), "index_save");
        assert_eq!(ReplayErrorKind::WorkdirReset.to_string(), "workdir_reset");
    }
}

async fn rebase_worktree_guard_structured(
    new_index: &git_internal::internal::index::Index,
    action: &str,
) -> Result<(), RebaseError> {
    let unstaged = status::changes_to_be_staged_with_policy(IgnorePolicy::Respect)
        .map_err(|err| RebaseError::WorktreeStatus(err.to_string()))?;
    if !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() {
        return Err(RebaseError::WorktreeDirty {
            action: action.to_string(),
            detail: "unstaged changes".to_string(),
        });
    }

    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|err| RebaseError::WorktreeStatus(err.to_string()))?;
    if !staged.new.is_empty() || !staged.modified.is_empty() || !staged.deleted.is_empty() {
        return Err(RebaseError::WorktreeDirty {
            action: action.to_string(),
            detail: "uncommitted changes".to_string(),
        });
    }

    if let Some(conflict) = worktree::untracked_overwrite_path(&unstaged.new, new_index) {
        return Err(RebaseError::UntrackedOverwrite {
            path: conflict.display().to_string(),
        });
    }

    Ok(())
}

/// Resolve a branch name or commit reference to a ObjectHash hash
///
/// This function first tries to find a branch with the given name,
/// then falls back to resolving it as a commit reference (hash, HEAD, etc.).
/// This allows the rebase command to work with both branch names and commit hashes.
async fn resolve_branch_or_commit(reference: &str) -> Result<ObjectHash, String> {
    util::get_commit_base(reference).await
}

#[derive(Debug, Copy, Clone)]
enum MergeResolution {
    Use(RebaseTreeEntry),
    Delete,
    Conflict(ConflictKind),
}

#[derive(Debug, Copy, Clone)]
enum ConflictKind {
    BothChanged {
        ours: ObjectHash,
        theirs: ObjectHash,
    },
    OursModifiedTheirsDeleted {
        ours: ObjectHash,
    },
    TheirsModifiedOursDeleted {
        theirs: ObjectHash,
    },
}

#[derive(Debug, Copy, Clone)]
enum RelativeState {
    Same(RebaseTreeEntry),
    Modified(RebaseTreeEntry),
    Deleted,
    Added(RebaseTreeEntry),
    Missing,
}

fn classify_relative_to_base(
    base: Option<&RebaseTreeEntry>,
    side: Option<&RebaseTreeEntry>,
) -> RelativeState {
    match (base, side) {
        (Some(b), Some(s)) if b == s => RelativeState::Same(*s),
        (Some(_), Some(s)) => RelativeState::Modified(*s),
        (Some(_), None) => RelativeState::Deleted,
        (None, Some(s)) => RelativeState::Added(*s),
        (None, None) => RelativeState::Missing,
    }
}

fn resolve_three_way(
    base: Option<&RebaseTreeEntry>,
    theirs: Option<&RebaseTreeEntry>,
    ours: Option<&RebaseTreeEntry>,
) -> MergeResolution {
    let base_present = base.is_some();
    let theirs_state = classify_relative_to_base(base, theirs);
    let ours_state = classify_relative_to_base(base, ours);

    match (base_present, ours_state, theirs_state) {
        (false, RelativeState::Missing, RelativeState::Missing) => MergeResolution::Delete,
        (false, RelativeState::Added(o), RelativeState::Missing) => MergeResolution::Use(o),
        (false, RelativeState::Missing, RelativeState::Added(t)) => MergeResolution::Use(t),
        (false, RelativeState::Added(o), RelativeState::Added(t)) => {
            if o == t {
                MergeResolution::Use(t)
            } else {
                MergeResolution::Conflict(ConflictKind::BothChanged {
                    ours: o.hash,
                    theirs: t.hash,
                })
            }
        }
        (true, RelativeState::Same(o), RelativeState::Same(_)) => MergeResolution::Use(o),
        (true, RelativeState::Same(_), RelativeState::Modified(t)) => MergeResolution::Use(t),
        (true, RelativeState::Modified(o), RelativeState::Same(_)) => MergeResolution::Use(o),
        (true, RelativeState::Modified(o), RelativeState::Modified(t)) => {
            if o == t {
                MergeResolution::Use(t)
            } else {
                MergeResolution::Conflict(ConflictKind::BothChanged {
                    ours: o.hash,
                    theirs: t.hash,
                })
            }
        }
        (true, RelativeState::Deleted, RelativeState::Same(_)) => MergeResolution::Delete,
        (true, RelativeState::Same(_), RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Modified(t)) => {
            MergeResolution::Conflict(ConflictKind::TheirsModifiedOursDeleted { theirs: t.hash })
        }
        (true, RelativeState::Modified(o), RelativeState::Deleted) => {
            MergeResolution::Conflict(ConflictKind::OursModifiedTheirsDeleted { ours: o.hash })
        }
        _ => {
            debug_assert!(false, "unexpected three-way merge state");
            MergeResolution::Delete
        }
    }
}

fn write_conflict_markers(
    workdir: &Path,
    path: &Path,
    marker_eol: &str,
    commit_abbrev: &str,
    kind: ConflictKind,
) -> Result<(), String> {
    match kind {
        ConflictKind::BothChanged { ours, theirs } => {
            let our_content = Blob::load(&ours).data;
            let their_content = Blob::load(&theirs).data;
            let our_text = conflict_payload(&our_content);
            let their_text = conflict_payload(&their_content);
            let conflict_content = format!(
                "<<<<<<< HEAD{marker_eol}{}{marker_eol}======={marker_eol}{}{marker_eol}>>>>>>> {}{marker_eol}",
                our_text, their_text, commit_abbrev
            );
            write_conflict_file(workdir, path, &conflict_content)?;
        }
        ConflictKind::OursModifiedTheirsDeleted { ours } => {
            let our_content = Blob::load(&ours).data;
            let our_text = conflict_payload(&our_content);
            let conflict_content = format!(
                "<<<<<<< HEAD{marker_eol}{}{marker_eol}======={marker_eol}>>>>>>> {} (deleted){marker_eol}",
                our_text, commit_abbrev
            );
            write_conflict_file(workdir, path, &conflict_content)?;
        }
        ConflictKind::TheirsModifiedOursDeleted { theirs } => {
            let their_content = Blob::load(&theirs).data;
            let their_text = conflict_payload(&their_content);
            let conflict_content = format!(
                "<<<<<<< HEAD (deleted){marker_eol}======={marker_eol}{}{marker_eol}>>>>>>> {}{marker_eol}",
                their_text, commit_abbrev
            );
            write_conflict_file(workdir, path, &conflict_content)?;
        }
    }
    Ok(())
}

/// Replay a single commit with conflict detection
///
/// This function performs a three-way merge to apply the changes from one commit
/// onto a different base commit, with proper conflict detection.
///
/// The three points of the merge are:
/// - Base: The original parent of the commit being replayed
/// - Theirs: The commit being replayed (contains the changes to apply)
/// - Ours: The new parent commit (where we want to apply the changes)
///
/// For each path, it compares the content in these three trees and constructs
/// a merged tree. If both `ours` and `theirs` modify the same path in
/// incompatible ways relative to `base`, the function reports a conflict
/// and leaves resolution to the caller.
async fn replay_commit_with_conflict_detection(
    commit_to_replay_id: &ObjectHash,
    new_parent_id: &ObjectHash,
    action: RebaseTodoAction,
    empty_mode: RebaseEmptyMode,
) -> ReplayResult {
    let index_file = path::index();
    let current_index = match git_internal::internal::index::Index::load(&index_file) {
        Ok(idx) => idx,
        Err(e) => {
            return ReplayResult::internal(ReplayErrorKind::IndexLoad, format!("{:?}", e));
        }
    };

    let commit_to_replay: Commit = match load_object(commit_to_replay_id) {
        Ok(c) => c,
        Err(e) => return ReplayResult::internal(ReplayErrorKind::CommitLoad, e.to_string()),
    };

    let original_parent_id = match commit_to_replay.parent_commit_ids.first() {
        Some(id) => id,
        None => {
            return ReplayResult::internal(ReplayErrorKind::MissingParent, "commit has no parents");
        }
    };

    // Load the three trees needed for the three-way merge
    let base_tree: Tree =
        match load_object::<Commit>(original_parent_id).and_then(|c| load_object(&c.tree_id)) {
            Ok(t) => t,
            Err(e) => return ReplayResult::internal(ReplayErrorKind::BaseTreeLoad, e.to_string()),
        };

    let their_tree: Tree = match load_object(&commit_to_replay.tree_id) {
        Ok(t) => t,
        Err(e) => return ReplayResult::internal(ReplayErrorKind::TheirTreeLoad, e.to_string()),
    };

    let our_tree: Tree =
        match load_object::<Commit>(new_parent_id).and_then(|c| load_object(&c.tree_id)) {
            Ok(t) => t,
            Err(e) => return ReplayResult::internal(ReplayErrorKind::OurTreeLoad, e.to_string()),
        };

    // Get all items from each tree and a union of their paths.
    let (tree_items, all_paths) =
        collect_tree_items_and_paths([&base_tree, &their_tree, &our_tree]);
    let base_items = &tree_items[0];
    let their_items = &tree_items[1];
    let our_items = &tree_items[2];

    let mut merged_items: HashMap<PathBuf, RebaseTreeEntry> = HashMap::new();
    let mut conflict_items: Vec<(PathBuf, ConflictKind)> = Vec::new();
    let workdir = util::working_dir();
    let commit_abbrev = commit_to_replay_id.to_string();
    let commit_short = &commit_abbrev[..7];
    let marker_eol = conflict_marker_eol();
    let untracked_paths = match worktree::untracked_workdir_paths(&current_index) {
        Ok(paths) => paths,
        Err(e) => return ReplayResult::internal(ReplayErrorKind::IndexLoad, e.to_string()),
    };

    for path in all_paths {
        let base_entry = base_items.get(&path);
        let their_entry = their_items.get(&path);
        let our_entry = our_items.get(&path);

        match resolve_three_way(base_entry, their_entry, our_entry) {
            MergeResolution::Use(entry) => {
                merged_items.insert(path, entry);
            }
            MergeResolution::Delete => {}
            MergeResolution::Conflict(kind) => {
                conflict_items.push((path, kind));
            }
        }
    }

    let conflicts: Vec<PathBuf> = conflict_items
        .iter()
        .map(|(path, _)| path.clone())
        .collect();

    if !conflicts.is_empty() {
        let mut untracked_conflict = None;
        for untracked in &untracked_paths {
            for path in conflicts.iter().chain(merged_items.keys()) {
                if worktree::paths_conflict(untracked, path) {
                    untracked_conflict = Some(untracked.clone());
                    break;
                }
            }
            if untracked_conflict.is_some() {
                break;
            }
        }
        if let Some(conflict) = untracked_conflict {
            return ReplayResult::internal(
                ReplayErrorKind::UntrackedOverwrite,
                format!(
                    "untracked working tree file would be overwritten by rebase: {}",
                    conflict.display()
                ),
            );
        }

        for (path, kind) in &conflict_items {
            if let Err(e) = write_conflict_markers(&workdir, path, marker_eol, commit_short, *kind)
            {
                return ReplayResult::internal(ReplayErrorKind::ConflictMarker, e);
            }
        }

        // Update index with conflict entries
        let index_file = path::index();
        let mut index = git_internal::internal::index::Index::new();

        // Add non-conflicting files at stage 0
        for (path, entry) in &merged_items {
            if let Err(e) = add_rebase_index_entry(&mut index, path, *entry, 0) {
                return ReplayResult::internal(ReplayErrorKind::IndexSave, e);
            }
        }

        // Add conflicting files at stages 1, 2, 3
        for path in &conflicts {
            // Stage 1: base version
            if let Some(base_entry) = base_items.get(path)
                && let Err(e) = add_rebase_index_entry(&mut index, path, *base_entry, 1)
            {
                return ReplayResult::internal(ReplayErrorKind::IndexSave, e);
            }

            // Stage 2: ours version
            if let Some(our_entry) = our_items.get(path)
                && let Err(e) = add_rebase_index_entry(&mut index, path, *our_entry, 2)
            {
                return ReplayResult::internal(ReplayErrorKind::IndexSave, e);
            }

            // Stage 3: theirs version
            if let Some(their_entry) = their_items.get(path)
                && let Err(e) = add_rebase_index_entry(&mut index, path, *their_entry, 3)
            {
                return ReplayResult::internal(ReplayErrorKind::IndexSave, e);
            }
        }

        if let Err(e) = index.save(&index_file) {
            return ReplayResult::Conflict {
                paths: conflicts,
                message: Some(format!("index save: {}", e)),
            };
        }

        // Update working directory for non-conflicting paths so users can see clean changes.
        let mut tracked_paths: HashSet<PathBuf> = HashSet::new();
        tracked_paths.extend(current_index.tracked_files());
        tracked_paths.extend(base_items.keys().cloned());
        tracked_paths.extend(their_items.keys().cloned());
        tracked_paths.extend(our_items.keys().cloned());

        let conflict_set: HashSet<PathBuf> = conflicts.iter().cloned().collect();

        for (path, entry) in &merged_items {
            if let Err(e) = write_rebase_workdir_entry(&workdir, path, *entry) {
                return ReplayResult::Conflict {
                    paths: conflicts,
                    message: Some(e),
                };
            }
        }

        for path in tracked_paths {
            if conflict_set.contains(&path) || merged_items.contains_key(&path) {
                continue;
            }
            let full_path = workdir.join(&path);
            if !full_path.exists() {
                continue;
            }
            if let Err(e) = fs::remove_file(&full_path) {
                return ReplayResult::Conflict {
                    paths: conflicts,
                    message: Some(format!("failed to remove {}: {}", full_path.display(), e)),
                };
            }
        }

        // rerere: record the preimage of each just-written conflict and replay a
        // recorded resolution if one matches. A no-op unless `rerere.enabled`;
        // staging of a replayed file follows `rerere.autoUpdate` (rebase does not
        // expose a per-invocation `--rerere-autoupdate`).
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update after rebase conflict failed: {error}");
        }
        return ReplayResult::conflict(conflicts);
    }

    // No conflicts - create the merged tree and commit
    let new_tree_id = match create_tree_from_items_map(&merged_items) {
        Ok(id) => id,
        Err(e) => return ReplayResult::internal(ReplayErrorKind::TreeCreate, e.to_string()),
    };

    // `--empty=drop`: a commit that BECOMES empty after replay (the merged tree
    // equals the new parent's tree — its changes are already on the new base) is
    // skipped. This is distinct from a commit that BEGINS empty (handled by
    // `--no-keep-empty` up front): `their_tree != base_tree` confirms the original
    // commit DID introduce a change, so emptiness arose from the replay. The
    // index/worktree already equal the new parent (new_tree == our_tree), so no
    // mutation is needed before skipping.
    if empty_mode == RebaseEmptyMode::Drop
        && new_tree_id == our_tree.id
        && their_tree.id != base_tree.id
    {
        let subject = commit_subject_from_message(&commit_to_replay.message);
        return ReplayResult::BecameEmptyDropped { subject };
    }

    let new_commit =
        match create_replayed_commit(&commit_to_replay, new_tree_id, *new_parent_id, action) {
            Ok(commit) => commit,
            Err(e) => return ReplayResult::internal(ReplayErrorKind::CommitLoad, e),
        };

    if let Err(e) = save_object(&new_commit, &new_commit.id) {
        return ReplayResult::internal(ReplayErrorKind::CommitSave, e.to_string());
    }

    // Update index and working directory
    let mut index = git_internal::internal::index::Index::new();
    let new_tree: Tree = match load_object(&new_tree_id) {
        Ok(tree) => tree,
        Err(e) => return ReplayResult::internal(ReplayErrorKind::NewTreeLoad, e.to_string()),
    };
    if let Err(e) = rebuild_index_from_tree(&new_tree, &mut index, "") {
        return ReplayResult::internal(ReplayErrorKind::IndexRebuild, e.to_string());
    }
    if let Err(e) = index.save(&index_file) {
        return ReplayResult::internal(ReplayErrorKind::IndexSave, e.to_string());
    }
    if let Err(e) = reset_workdir_tracked_only(&current_index, &index) {
        return ReplayResult::internal(ReplayErrorKind::WorkdirReset, e.to_string());
    }

    ReplayResult::Success(new_commit.id)
}

fn create_replayed_commit(
    original_commit: &Commit,
    tree_id: ObjectHash,
    new_parent_id: ObjectHash,
    action: RebaseTodoAction,
) -> Result<Commit, String> {
    match action {
        RebaseTodoAction::Pick => Ok(Commit::from_tree_id(
            tree_id,
            vec![new_parent_id],
            &original_commit.message,
        )),
        RebaseTodoAction::Fixup => {
            let target: Commit = load_object(&new_parent_id).map_err(|error| error.to_string())?;
            Ok(Commit::from_tree_id(
                tree_id,
                target.parent_commit_ids.clone(),
                &target.message,
            ))
        }
        RebaseTodoAction::Squash => {
            let target: Commit = load_object(&new_parent_id).map_err(|error| error.to_string())?;
            let mut message = target.message.clone();
            message.push_str("\n\n");
            message.push_str(original_commit.message.trim());
            Ok(Commit::from_tree_id(
                tree_id,
                target.parent_commit_ids.clone(),
                &message,
            ))
        }
        RebaseTodoAction::Amend => {
            let target: Commit = load_object(&new_parent_id).map_err(|error| error.to_string())?;
            let message = amend_replacement_message(&original_commit.message);
            Ok(Commit::from_tree_id(
                tree_id,
                target.parent_commit_ids.clone(),
                &message,
            ))
        }
    }
}

fn amend_replacement_message(message: &str) -> String {
    let (clean_message, gpg_sig) = parse_commit_msg(message);
    let subject = clean_message.lines().next().unwrap_or("");
    if !subject.starts_with("amend! ") {
        return message.to_string();
    }
    let replacement = clean_message
        .split_once('\n')
        .map(|(_, replacement)| replacement.trim_start_matches('\n'))
        .unwrap_or_default();
    match gpg_sig {
        Some(signature) => format_commit_msg(replacement, Some(&format!("gpgsig {signature}"))),
        None => format_commit_msg(replacement, None),
    }
}

/// Collect all commits from base (exclusive) to head (inclusive) that need to be replayed
///
/// This function walks backwards from the head commit to the base commit,
/// collecting all commits in between. These are the commits that will be
/// Whether `commit_id` is empty in the original history — i.e. it introduces no
/// change relative to its first parent (its tree equals the parent's tree). A
/// root commit (no parent) is empty iff its tree has no entries. Used by
/// `rebase --no-keep-empty` to drop such commits before replay. A load failure
/// conservatively reports `false` (keep the commit) so a transient error never
/// silently discards work.
async fn commit_starts_empty(commit_id: &ObjectHash) -> bool {
    let Ok(commit) = load_object::<Commit>(commit_id) else {
        return false;
    };
    match commit.parent_commit_ids.first() {
        Some(parent_id) => match load_object::<Commit>(parent_id) {
            Ok(parent) => commit.tree_id == parent.tree_id,
            Err(_) => false,
        },
        None => load_object::<Tree>(&commit.tree_id)
            .map(|tree| tree.tree_items.is_empty())
            .unwrap_or(false),
    }
}

/// replayed onto the new upstream base.
///
/// The commits are returned in chronological order (oldest first) so they
/// can be replayed in the correct sequence.
async fn collect_commits_to_replay(
    base_id: &ObjectHash,
    head_id: &ObjectHash,
) -> Result<Vec<ObjectHash>, String> {
    // The shared-history boundary: the base and every one of its ancestors.
    // Stopping the first-parent walk at the FIRST commit already reachable from
    // the base (rather than only at `base_id` exactly) keeps a base that is not
    // on head's first-parent chain — a multiple-LCA criss-cross merge base —
    // from overshooting toward the root and replaying shared commits.
    let base_history: HashSet<ObjectHash> =
        crate::command::log::get_reachable_commits(base_id.to_string(), None)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|commit| commit.id)
            .collect();

    let mut commits = Vec::new();
    let mut current_id = *head_id;

    // Walk backwards from head, collecting commits until the shared history.
    while !base_history.contains(&current_id) {
        commits.push(current_id);
        let commit: Commit = load_object(&current_id).map_err(|e| e.to_string())?;
        if commit.parent_commit_ids.is_empty() {
            break; // Reached a root without meeting the base's history
        }
        current_id = commit.parent_commit_ids[0]; // Follow first parent
        // TODO: Handle merge commits properly - currently only follows first parent
        // This may miss commits in complex branch histories
    }

    // Reverse to get chronological order (oldest first)
    commits.reverse();
    Ok(commits)
}

/// Create a tree object from a flat map of file paths to content hashes
///
/// This function takes a HashMap of file paths and their content hashes,
/// and builds a proper Git tree structure. It handles:
/// - Grouping files by their parent directories
/// - Creating tree objects for each directory
/// - Recursively building the tree structure from root to leaves
///
/// Returns the ObjectHash hash of the root tree object.
fn create_tree_from_items_map(
    items: &HashMap<PathBuf, RebaseTreeEntry>,
) -> Result<ObjectHash, String> {
    // Group files by their parent directories
    let mut entries_map: HashMap<PathBuf, Vec<TreeItem>> = HashMap::new();
    for (path, entry) in items {
        let item = TreeItem {
            mode: entry.mode,
            name: tree_item_name(path)?,
            id: entry.hash,
        };
        let parent_dir = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        entries_map.entry(parent_dir).or_default().push(item);
    }
    build_tree_recursively(Path::new(""), &mut entries_map)
}

/// Recursively build tree objects from a directory structure
///
/// This helper function processes a directory and all its subdirectories:
/// 1. Creates tree items for all files in the current directory
/// 2. Recursively processes subdirectories to create subtree objects  
/// 3. Combines files and subdirectories into a single tree object
/// 4. Saves the tree object and returns its hash
///
/// The algorithm works bottom-up, creating leaf trees first and then
/// combining them into parent trees.
fn build_tree_recursively(
    current_path: &Path,
    entries_map: &mut HashMap<PathBuf, Vec<TreeItem>>,
) -> Result<ObjectHash, String> {
    // Get all files/items in the current directory
    let mut current_items = entries_map.remove(current_path).unwrap_or_default();

    // Find all subdirectories that are children of current directory
    let subdirs: Vec<_> = entries_map
        .keys()
        .filter(|p| p.parent() == Some(current_path))
        .cloned()
        .collect();

    // Recursively process each subdirectory
    for subdir_path in subdirs {
        let subdir_name = tree_item_name(&subdir_path)?;

        let subtree_hash = build_tree_recursively(&subdir_path, entries_map)?;

        // Add the subdirectory as a tree item
        current_items.push(TreeItem {
            mode: TreeItemMode::Tree,
            name: subdir_name,
            id: subtree_hash,
        });
    }

    crate::utils::tree::sort_tree_items_for_git(&mut current_items);
    // Create and save the tree object for this directory
    let tree = Tree::from_tree_items(current_items).map_err(|e| e.to_string())?;
    save_object(&tree, &tree.id).map_err(|e| e.to_string())?;
    Ok(tree.id)
}

/// Reset the working directory to match the new index state without overwriting untracked files.
fn reset_workdir_tracked_only(
    current_index: &git_internal::internal::index::Index,
    new_index: &git_internal::internal::index::Index,
) -> Result<(), String> {
    let workdir = util::working_dir();
    let untracked_paths = worktree::untracked_workdir_paths(current_index)?;
    if let Some(conflict) = worktree::untracked_overwrite_path(&untracked_paths, new_index) {
        return Err(format!(
            "untracked working tree file would be overwritten: {}",
            conflict.display()
        ));
    }
    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();

    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            let full_path = workdir.join(path_buf);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|e| e.to_string())?;
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        let path_str = path_to_index_key(&path_buf)?;
        if let Some(entry) = new_index.get(path_str, 0) {
            let mode = index_mode_to_tree_item_mode(entry.mode)?;
            write_rebase_workdir_entry(
                &workdir,
                &path_buf,
                RebaseTreeEntry {
                    hash: entry.hash,
                    mode,
                },
            )?;
        }
    }

    Ok(())
}

fn tree_item_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("path has no file name: {}", path.display()))?;
    name.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn path_to_index_key(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn add_rebase_index_entry(
    index: &mut git_internal::internal::index::Index,
    path: &Path,
    item: RebaseTreeEntry,
    stage: u8,
) -> Result<(), String> {
    let blob: Blob = load_object(&item.hash).map_err(|error| {
        format!(
            "failed to load blob {} for index entry '{}': {error}",
            item.hash,
            path.display()
        )
    })?;
    let mut entry = git_internal::internal::index::IndexEntry::new_from_blob(
        path_to_index_key(path)?.to_string(),
        item.hash,
        blob.data.len() as u32,
    );
    entry.mode = tree_item_mode_to_index_mode(item.mode)?;
    entry.flags.stage = stage;
    index.add(entry);
    Ok(())
}

fn tree_item_mode_to_index_mode(mode: TreeItemMode) -> Result<u32, String> {
    match mode {
        TreeItemMode::Blob => Ok(0o100644),
        TreeItemMode::BlobExecutable => Ok(0o100755),
        TreeItemMode::Link => Ok(0o120000),
        TreeItemMode::Tree => {
            Err("tree entry cannot be represented as a file index entry".to_string())
        }
        TreeItemMode::Commit => Err("gitlink entries are not supported by rebase".to_string()),
    }
}

fn index_mode_to_tree_item_mode(mode: u32) -> Result<TreeItemMode, String> {
    match mode {
        0o100644 => Ok(TreeItemMode::Blob),
        0o100755 => Ok(TreeItemMode::BlobExecutable),
        0o120000 => Ok(TreeItemMode::Link),
        other => Err(format!(
            "unsupported index mode {other:o} while creating rebase tree"
        )),
    }
}

/// Rebuild an index from a tree object by recursively adding all files
///
/// This function traverses a tree object and adds all files to the given index.
/// It handles both files (blobs) and subdirectories (trees) by:
/// 1. For files: Loading the blob and creating an index entry
/// 2. For subdirectories: Recursively processing the subtree
///
/// The prefix parameter tracks the current directory path during recursion.
fn rebuild_index_from_tree(
    tree: &Tree,
    index: &mut git_internal::internal::index::Index,
    prefix: &str,
) -> Result<(), String> {
    for item in &tree.tree_items {
        let full_path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{}/{}", prefix, item.name)
        };

        let index_mode = match item.mode {
            git_internal::internal::object::tree::TreeItemMode::Tree => {
                let subtree: Tree = load_object(&item.id).map_err(|e| {
                    format!(
                        "failed to load tree {} for rebase index entry '{}': {e}",
                        item.id, full_path
                    )
                })?;
                rebuild_index_from_tree(&subtree, index, &full_path)?;
                continue;
            }
            git_internal::internal::object::tree::TreeItemMode::Blob => 0o100644,
            git_internal::internal::object::tree::TreeItemMode::BlobExecutable => 0o100755,
            git_internal::internal::object::tree::TreeItemMode::Link => 0o120000,
            git_internal::internal::object::tree::TreeItemMode::Commit => {
                return Err(format!(
                    "unsupported gitlink tree entry '{}' while rebuilding rebase index",
                    full_path
                ));
            }
        };

        let blob: git_internal::internal::object::blob::Blob =
            load_object(&item.id).map_err(|e| {
                format!(
                    "failed to load blob {} for rebase index entry '{}': {e}",
                    item.id, full_path
                )
            })?;
        let mut entry = git_internal::internal::index::IndexEntry::new_from_blob(
            full_path,
            item.id,
            blob.data.len() as u32,
        );
        entry.mode = index_mode;
        index.add(entry);
    }
    Ok(())
}

#[cfg(test)]
mod rebuild_index_tests {
    use std::str::FromStr;

    use git_internal::{
        hash::ObjectHash,
        internal::{
            index::Index,
            object::{
                blob::Blob,
                tree::{Tree, TreeItem, TreeItemMode},
            },
        },
    };
    use tempfile::tempdir;

    use super::rebuild_index_from_tree;
    use crate::{
        command::save_object,
        utils::test::{ChangeDirGuard, setup_with_new_libra_in},
    };

    #[tokio::test]
    #[serial_test::serial]
    async fn rebuild_index_from_tree_preserves_executable_and_symlink_modes() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let executable_blob = Blob::from_content("run\n");
        save_object(&executable_blob, &executable_blob.id).unwrap();
        let symlink_blob = Blob::from_content("target.txt");
        save_object(&symlink_blob, &symlink_blob.id).unwrap();
        let regular_blob = Blob::from_content("plain\n");
        save_object(&regular_blob, &regular_blob.id).unwrap();

        let tree = Tree::from_tree_items(vec![
            TreeItem::new(
                TreeItemMode::BlobExecutable,
                executable_blob.id,
                "run.sh".to_string(),
            ),
            TreeItem::new(TreeItemMode::Link, symlink_blob.id, "link".to_string()),
            TreeItem::new(TreeItemMode::Blob, regular_blob.id, "plain.txt".to_string()),
        ])
        .unwrap();
        let mut index = Index::new();

        rebuild_index_from_tree(&tree, &mut index, "").unwrap();

        assert_eq!(index.get("run.sh", 0).unwrap().mode, 0o100755);
        assert_eq!(index.get("link", 0).unwrap().mode, 0o120000);
        assert_eq!(index.get("plain.txt", 0).unwrap().mode, 0o100644);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn rebuild_index_from_tree_returns_path_context_for_missing_blob() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let missing_blob =
            ObjectHash::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            missing_blob,
            "missing.txt".to_string(),
        )])
        .unwrap();
        let mut index = Index::new();

        let err = rebuild_index_from_tree(&tree, &mut index, "").unwrap_err();

        assert!(err.contains("failed to load blob"));
        assert!(err.contains("missing.txt"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn rebuild_index_from_tree_rejects_gitlink_entries() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let gitlink = ObjectHash::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Commit,
            gitlink,
            "vendor".to_string(),
        )])
        .unwrap();
        let mut index = Index::new();

        let err = rebuild_index_from_tree(&tree, &mut index, "").unwrap_err();

        assert!(err.contains("unsupported gitlink tree entry"));
        assert!(err.contains("vendor"));
    }
}
