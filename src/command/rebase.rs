//! Rebase implementation that parses onto/branch arguments, replays commits onto a new base, handles conflicts, and updates branch refs.

use std::{
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
        tree::{Tree, TreeItemMode},
    },
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, Statement, Value,
};
use serde::{Deserialize, Serialize};

use crate::{
    cli_error,
    command::{editor, load_object, merge, rebase_todo, save_object, status, switch},
    common_utils::{format_commit_msg, parse_commit_msg},
    internal::{
        branch::Branch,
        change::{
            RelationKind,
            record_current_repo_commit_revision_with_predecessors_for_active_operation,
        },
        head::Head,
        model::{reference as ref_model, reflog as reflog_model},
        reflog,
        reflog::{ReflogAction, ReflogContext, ReflogError, with_reflog},
        repo_hooks::{RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        ignore::IgnorePolicy,
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
pub(crate) struct RebaseAuxState {
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
    /// Explicit rerere staging choice for this rebase. Missing fields from
    /// older sidecars inherit the current `rerere.autoUpdate` configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerere_autoupdate: Option<bool>,
    /// Remaining interactive instructions (HF-21 / ADR-HF-19). Additive so
    /// older sidecars remain readable; empty when the rebase is not interactive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) todo_instructions: Vec<rebase_todo::TodoInstruction>,
    /// Instructions already applied during an interactive rebase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) done_instructions: Vec<rebase_todo::TodoInstruction>,
    /// Set when the sequence editor produced an invalid todo (HF-28 / I9).
    /// `--continue` refuses until HF-23 `--edit-todo` rewrites the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) interactive_parse_error: Option<String>,
    /// Edited todo text retained after an invalid-line halt (HF-23 `--edit-todo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) interactive_todo_text: Option<String>,
    /// Original replay-range commit ids for abbrev resolve after `--edit-todo`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    interactive_known: Vec<String>,
    /// Why an interactive rebase is paused: `edit`, `break`, or `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interactive_stop: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RebaseRefUpdate {
    branch: String,
    old_oid: String,
}

impl RebaseAuxState {
    /// Part C W1 (§C.4.2): the aux sidecar (exec queue, update-refs plan,
    /// rewrites, held autostash oid) is per-rebase state, so it lives in THIS
    /// worktree's local gitdir. For the main worktree the local gitdir IS the
    /// common `.libra`, so main-worktree paths are unchanged.
    fn path() -> PathBuf {
        util::request_worktree_gitdir_strict().join("rebase-aux.json")
    }

    fn load_optional() -> Result<Option<Self>, RebaseError> {
        Self::load_optional_at(&Self::path())
    }

    fn load_optional_at(path: &std::path::Path) -> Result<Option<Self>, RebaseError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path).map_err(|error| RebaseError::AuxStateLoad {
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

    #[cfg(test)]
    pub(crate) fn with_todo_instructions(
        todo_instructions: Vec<rebase_todo::TodoInstruction>,
        done_instructions: Vec<rebase_todo::TodoInstruction>,
    ) -> Self {
        Self {
            todo_instructions,
            done_instructions,
            ..Self::default()
        }
    }

    fn marks_interactive(&self) -> bool {
        !self.todo_instructions.is_empty()
            || !self.done_instructions.is_empty()
            || self.interactive_parse_error.is_some()
            || self.interactive_todo_text.is_some()
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
///
/// Scope (Part C §C.9): GC enumerates EVERY worktree's gitdir, so this reads
/// the aux sidecar of the gitdir the caller names — a held autostash is a
/// first-class reachability root regardless of which worktree holds it.
/// The SEMANTIC OID fields of this gitdir's `rebase-aux.json`, for GC root
/// collection (plan-20260714 §C.4.3): the held autostash, every update-refs
/// plan tip, and BOTH sides of every rewrite (including the map KEYS — the
/// original commits — which a generic JSON scan would miss entirely).
/// `exec_commands` and branch names are text and are NOT returned.
pub(crate) fn rebase_aux_gc_oids(
    gitdir: &std::path::Path,
) -> Result<Option<Vec<(&'static str, String)>>, String> {
    let aux = RebaseAuxState::load_optional_at(&gitdir.join("rebase-aux.json"))
        .map_err(|error| error.to_string())?;
    let Some(aux) = aux else {
        return Ok(None);
    };
    let mut oids = Vec::new();
    if let Some(autostash) = aux.autostash {
        oids.push(("autostash", autostash));
    }
    for update in aux.refs_to_update {
        oids.push(("refs_to_update.old_oid", update.old_oid));
    }
    for (original, rewritten) in aux.rewrites {
        oids.push(("rewrites (original)", original));
        oids.push(("rewrites (rewritten)", rewritten));
    }
    for (original, parent) in aux.rewrite_aliases {
        oids.push(("rewrite_aliases (original)", original));
        oids.push(("rewrite_aliases (parent)", parent));
    }
    for instruction in &aux.todo_instructions {
        if let Some(oid) = instruction.commit_oid() {
            oids.push(("todo_instructions", oid.to_string()));
        }
    }
    for instruction in &aux.done_instructions {
        if let Some(oid) = instruction.commit_oid() {
            oids.push(("done_instructions", oid.to_string()));
        }
    }
    Ok(Some(oids))
}

pub(crate) fn held_autostash_oid_in_gitdir(
    gitdir: &std::path::Path,
) -> CliResult<Option<ObjectHash>> {
    RebaseAuxState::load_optional_at(&gitdir.join("rebase-aux.json"))
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
    /// The legacy common-storage rebase directory Git writes as `rebase-merge`
    /// (interactive) or `rebase-apply` (am-based). Both are recognized: §C.4.3
    /// names both, and recognizing only one meant `--continue`/`--abort`
    /// reported "no rebase" while the sequencer's broader probe still blocked a
    /// new start — the worst of both answers.
    const LEGACY_REBASE_DIRS: [&'static str; 2] = ["rebase-merge", "rebase-apply"];

    /// The legacy directory that EXISTS, if any (checked in the order above,
    /// so `rebase-merge` wins when a repository somehow has both).
    fn legacy_rebase_dir_present() -> Option<PathBuf> {
        let storage = util::request_storage_path();
        Self::LEGACY_REBASE_DIRS
            .iter()
            .map(|name| storage.join(name))
            .find(|path| path.exists())
    }

    fn legacy_rebase_dir() -> PathBuf {
        Self::legacy_rebase_dir_present()
            .unwrap_or_else(|| util::request_storage_path().join(Self::LEGACY_REBASE_DIRS[0]))
    }

    /// Check if a rebase is in progress.
    ///
    /// A READ, and reads never consume legacy state (§C.4.2 / ADR-0714-08):
    /// the presence of a legacy `rebase-merge/` directory is REPORTED, not
    /// adopted. `libra status` asking whether a rebase is in progress must not
    /// be the thing that migrates and deletes crash-recovery state whose owner
    /// it has not established.
    pub async fn is_in_progress() -> Result<bool, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        if Self::has_state_in_db(&db).await? {
            return Ok(true);
        }
        Self::legacy_state_is_adoptable()
    }

    /// Whether a legacy directory exists that THIS worktree could adopt.
    ///
    /// Read-only. Refuses (as an error) when the owner is ambiguous, so a
    /// caller reports the situation instead of guessing — and returns `false`
    /// for a linked worktree, because a common-storage directory is not its
    /// rebase.
    fn legacy_state_is_adoptable() -> Result<bool, String> {
        let Some(legacy_dir) = Self::legacy_rebase_dir_present() else {
            return Ok(false);
        };
        if crate::internal::worktree_scope::WorktreeScope::for_request().is_linked() {
            return Ok(false);
        }
        // EVER registered, not merely currently registered (§C.4.3): a linked
        // worktree that has since been removed leaves no entry, and this
        // directory could have been its rebase.
        if crate::command::maintenance::repository_had_linked_worktrees() {
            return Err(Self::ambiguous_legacy_message(&legacy_dir));
        }
        Ok(true)
    }

    fn ambiguous_legacy_message(legacy_dir: &Path) -> String {
        format!(
            "a legacy rebase state directory exists at '{}' but linked worktrees are \
             registered, so its owner is ambiguous and it will not be adopted automatically; \
             finish or abort that legacy rebase, or remove the directory manually once you \
             have confirmed it is stale",
            legacy_dir.display()
        )
    }

    /// Save rebase state to the database.
    ///
    /// One TRANSACTION around the scoped delete and the insert: a failure
    /// between them would leave this worktree with no rebase state at all —
    /// and callers have already moved HEAD by then, so the user would be
    /// mid-rebase with nothing to continue or abort.
    pub async fn save(&self) -> Result<(), String> {
        use sea_orm::TransactionTrait;

        let db = crate::internal::sequencer::request_db_checked().await?;
        let txn = db
            .begin()
            .await
            .map_err(|error| format!("failed to begin the rebase_state transaction: {error}"))?;
        Self::save_with_conn(&txn, self).await?;
        txn.commit()
            .await
            .map_err(|error| format!("failed to commit the rebase_state transaction: {error}"))
    }

    /// The FIRST write of a starting rebase, as an atomic claim (§C.4.4).
    ///
    /// [`Self::save`] is a scoped DELETE + INSERT — correct for an owner
    /// advancing its own rebase, wrong for a start: two starts racing in one
    /// worktree both pass the mutex check (nothing is in progress yet) and the
    /// loser's replace erases the winner's todo while the winner's checkout
    /// stays on disk. A bare INSERT against `worktree_id PRIMARY KEY` lets
    /// exactly one starter win.
    pub async fn claim_start(&self) -> Result<(), String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        match Self::insert_with_conn(&db, self).await {
            Ok(()) => Ok(()),
            Err(err) if crate::internal::sequencer::is_unique_violation_text(&err) => {
                Err("a rebase is already in progress in this worktree".to_string())
            }
            Err(err) => Err(err),
        }
    }

    /// Load rebase state.
    ///
    /// Reads the scoped row, then — for the main worktree with no ambiguity —
    /// READS a legacy directory without adopting it: no DB row is written and
    /// nothing is deleted (§C.4.2 / ADR-0714-08). Adoption is an explicit act,
    /// performed by a control action through [`Self::adopt_legacy_state`].
    pub async fn load() -> Result<Self, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        if let Some(state) = Self::load_from_db(&db).await? {
            return Ok(state);
        }
        if Self::legacy_state_is_adoptable()? {
            return Self::load_from_legacy_dir();
        }
        Err("No rebase in progress".to_string())
    }

    /// EXPLICITLY adopt a legacy directory into this worktree's scoped row.
    ///
    /// Called only from a control action (`--continue` / `--skip` / `--abort`),
    /// where the user has said "act on this rebase" — never from a read. The
    /// same ambiguity rule applies: a linked worktree never adopts, and the
    /// main worktree refuses while linked worktrees are registered.
    pub(crate) async fn adopt_legacy_state() -> Result<Option<Self>, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::migrate_legacy_state(&db).await
    }

    /// Remove the rebase state from the database (and any legacy state on disk)
    pub async fn cleanup() -> Result<(), String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::clear_state_in_db(&db).await?;

        // The COMMON legacy dir has no owner metadata, and the SAME
        // ambiguity rule that governs adopting it governs deleting it
        // (§C.4.2 / ADR-0714-08). A linked worktree's cleanup clears only its
        // own DB row above; and the main worktree may delete the directory
        // only when it is the unambiguous owner — with linked worktrees
        // registered, `migrate_legacy_state` refuses to ADOPT it precisely
        // because it might be theirs, so `--abort` must not destroy it
        // either. It is left for `worktree doctor` / an explicit removal.
        let scope_is_linked =
            crate::internal::worktree_scope::WorktreeScope::for_request().is_linked();
        if !scope_is_linked && Self::legacy_rebase_dir_present().is_some() {
            // Destructive, so the same lock and the same DURABLE evidence as
            // adoption: `repository_has_linked_worktrees` (registered NOW) is
            // not enough — a linked worktree removed earlier leaves no entry,
            // and this directory could have been its rebase (§C.4.3).
            let _registry = crate::command::worktree::acquire_registry_lock_async()
                .await
                .map_err(|error| format!("cannot take the worktree registry lock: {error}"))?;
            let legacy_dir = Self::legacy_rebase_dir();
            if legacy_dir.exists() {
                if crate::command::maintenance::repository_had_linked_worktrees() {
                    emit_warning(format!(
                        "a legacy rebase state directory remains at '{}': linked worktrees are \
                         registered, so its owner is ambiguous and it was NOT removed. Remove it \
                         manually once you have confirmed it is stale.",
                        legacy_dir.display()
                    ));
                } else {
                    fs::remove_dir_all(&legacy_dir).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// The current worktree's `rebase_state` scope key (Part C W1, §C.4.2):
    /// main worktree = `""`, a linked worktree = its stable instance id —
    /// the `worktree_id TEXT NOT NULL` storage convention shared with
    /// `sequence_state`/`bisect_state`.
    fn scope_key() -> String {
        crate::internal::worktree_scope::WorktreeScope::for_request()
            .storage_key()
            .to_string()
    }

    // W1, §C.11 "clear the lazy DDL": `rebase_state` is created by migration
    // `2026072101_rebase_state_worktree_scope`, which every connection open
    // applies before any command runs, so nothing here creates it. The read
    // path used to `CREATE TABLE IF NOT EXISTS` on every call — DDL on a
    // READ, taking SQLite's schema lock, and quietly papering over a database
    // that never got migrated. A missing table now surfaces as the storage
    // error it is.

    async fn has_state_in_db<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT 1 FROM rebase_state WHERE worktree_id = ? LIMIT 1;",
            [Self::scope_key().into()],
        );
        let row = db
            .query_one_raw(stmt)
            .await
            .map_err(|e| format!("failed to query rebase_state: {e}"))?;
        Ok(row.is_some())
    }

    /// The row of an EXPLICITLY resolved scope (§C.4.2), for the pseudo-ref
    /// projections. Reads only the database — a legacy common directory is
    /// deliberately not adopted here, because §C.4.3 forbids attributing it to
    /// a scope that cannot be proven to own it.
    pub(crate) async fn load_for_scope(
        scope: &crate::internal::worktree_scope::WorktreeScope,
    ) -> Result<Option<Self>, String> {
        let db = crate::internal::sequencer::request_db_checked().await?;
        Self::load_from_db_in_scope(&db, scope.storage_key()).await
    }

    async fn load_from_db<C: ConnectionTrait>(db: &C) -> Result<Option<Self>, String> {
        Self::load_from_db_in_scope(db, &Self::scope_key()).await
    }

    async fn load_from_db_in_scope<C: ConnectionTrait>(
        db: &C,
        scope_key: &str,
    ) -> Result<Option<Self>, String> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
                SELECT head_name, onto, orig_head, current_head, todo, done, stopped_sha, autosquash, todo_actions, empty_mode
                FROM rebase_state
                WHERE worktree_id = ?
                LIMIT 1
            "#,
            [scope_key.into()],
        );
        let row = db
            .query_one_raw(stmt)
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
        // Part C W1 (§C.4.2): scoped DELETE — never the whole table, which
        // would clobber another worktree's in-progress rebase.
        let delete_stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM rebase_state WHERE worktree_id = ?;",
            [Self::scope_key().into()],
        );
        db.execute_raw(delete_stmt)
            .await
            .map_err(|e| format!("failed to clear existing rebase_state: {e}"))?;
        Self::insert_with_conn(db, state).await
    }

    /// The INSERT half of [`Self::save_with_conn`], without the scoped delete
    /// — so a STARTING rebase can claim the slot instead of replacing it.
    async fn insert_with_conn<C: ConnectionTrait>(
        db: &C,
        state: &RebaseState,
    ) -> Result<(), String> {
        let todo = Self::format_hash_list(state.todo.iter().cloned());
        let todo_actions_body = if state.todo_actions.len() == state.todo.len() {
            Self::format_action_list(state.todo_actions.iter().copied())
        } else {
            Self::format_action_list(
                Self::default_todo_actions(&state.todo, state.autosquash)
                    .iter()
                    .copied(),
            )
        };
        let todo_actions = encode_todo_actions_blob(todo_actions_body, rebase_aux_is_interactive());
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
                (worktree_id, head_name, onto, orig_head, current_head, todo, todo_actions, done, stopped_sha, autosquash, empty_mode)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
            "#,
            [
                Self::scope_key().into(),
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

        db.execute_raw(insert_stmt)
            .await
            .map_err(|e| format!("failed to save rebase_state: {e}"))?;
        Ok(())
    }

    async fn clear_state_in_db<C: ConnectionTrait>(db: &C) -> Result<(), String> {
        // Part C W1 (§C.4.2): clear only THIS worktree's row.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM rebase_state WHERE worktree_id = ?;",
            [Self::scope_key().into()],
        );
        db.execute_raw(stmt)
            .await
            .map_err(|e| format!("failed to clear rebase_state: {e}"))?;
        Ok(())
    }

    async fn migrate_legacy_state<C: ConnectionTrait>(db: &C) -> Result<Option<Self>, String> {
        if Self::legacy_rebase_dir_present().is_none() {
            return Ok(None);
        }
        // The REGISTRY LOCK wraps the whole decision: a concurrent `worktree
        // add` between the ambiguity check and the unlink would make this
        // directory ambiguous after we had already decided it was not. Taken
        // before the checks, and every check re-run inside it — the probe above
        // is only a cheap early exit.
        let _registry = crate::command::worktree::acquire_registry_lock_async()
            .await
            .map_err(|error| format!("cannot take the worktree registry lock: {error}"))?;
        let Some(legacy_dir) = Self::legacy_rebase_dir_present() else {
            // Another process adopted it while we waited for the lock.
            return Ok(None);
        };

        // Part C W1 (§C.4.2 ambiguous-common-sidecar rule): the legacy
        // `rebase-merge/` directory lives in COMMON storage with no owner
        // metadata. A linked worktree must never adopt it (it is not this
        // worktree's rebase — same reasoning as the sequencer mutex's
        // main-only legacy probes), and even the main worktree must not
        // consume it while linked worktrees are registered: with more than
        // one candidate owner, adopting-and-destroying here could wipe
        // another worktree's crash-recovery state.
        if crate::internal::worktree_scope::WorktreeScope::for_request().is_linked() {
            return Ok(None);
        }
        if crate::command::maintenance::repository_had_linked_worktrees() {
            return Err(Self::ambiguous_legacy_message(&legacy_dir));
        }

        let state = Self::load_from_legacy_dir()?;
        Self::save_with_conn(db, &state).await?;
        if let Err(e) = fs::remove_dir_all(&legacy_dir) {
            emit_warning(format!("failed to remove legacy rebase state: {e}"));
        }
        Ok(Some(state))
    }

    fn load_from_legacy_dir() -> Result<Self, String> {
        let Some(dir) = Self::legacy_rebase_dir_present() else {
            return Err("No rebase in progress".to_string());
        };

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
        let (_interactive, tokens) = decode_todo_actions_blob(content);
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
/// [`replay_commit_with_unified_merge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayErrorKind {
    IndexLoad,
    CommitLoad,
    /// Retained for Display/JSON pins. Parentless commits now replay via an
    /// empty-base merge (`--root --onto`) or object reuse (`--root`).
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
    /// No `user.name` / `user.email` to sign the replayed commit's committer with.
    IdentityMissing,
    /// A replayed commit's three-way inputs carry a gitlink the rebase would
    /// have to arbitrate — refused fail-closed (ADR-MG-01).
    GitlinkUnsupported,
    /// The shared merge tree engine could not prepare or materialize the
    /// replay. The original error detail remains user-visible.
    MergeEngine,
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
            ReplayErrorKind::IdentityMissing => "identity_missing",
            ReplayErrorKind::GitlinkUnsupported => "gitlink_unsupported",
            ReplayErrorKind::MergeEngine => "merge_engine",
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
            ReplayErrorKind::IdentityMissing => StableErrorCode::AuthMissingCredentials,
            ReplayErrorKind::GitlinkUnsupported => StableErrorCode::Unsupported,
            ReplayErrorKind::MergeEngine => StableErrorCode::RepoStateInvalid,
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
    libra rebase --no-autosquash main  Replay without folding fixup!/squash! commits
    libra rebase --root               Replay every commit from the root commit
    libra rebase --root --onto main   Replay the full history onto main
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
    libra rebase -i main          Interactive rebase (sequence editor)
    libra rebase --edit-todo      Edit remaining commands of an in-progress interactive rebase
    libra rebase --json main      Structured JSON output for agents";

/// Command-line arguments for the rebase operation
#[derive(Parser, Debug)]
#[command(after_help = REBASE_EXAMPLES)]
pub struct RebaseArgs {
    /// The upstream branch to rebase the current branch onto.
    /// This can be a branch name, commit hash, or other Git reference.
    #[clap(required_unless_present_any = ["continue_rebase", "abort", "skip", "root", "edit_todo"])]
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

    /// Replay every commit from the root commit. The optional positional is
    /// `<branch>` (checked out first), not `<upstream>`. Combined with an
    /// `<upstream>` positional it is a usage error. Without `--onto` the root
    /// is replayed as a parentless commit; unchanged picks keep their hashes.
    #[clap(long, conflicts_with_all = ["continue_rebase", "abort", "skip"])]
    pub root: bool,

    /// Move fixup!/squash! commits next to their targets and fold them while
    /// replaying. Explicit `--autosquash` also skips the already-up-to-date
    /// shortcut so a linear history still folds. Last one wins against
    /// `--no-autosquash`. The `rebase.autosquash` config is ignored for
    /// non-interactive rebase (Git `t3415`).
    #[clap(
        long,
        overrides_with = "no_autosquash",
        conflicts_with_all = ["continue_rebase", "abort", "skip"]
    )]
    pub autosquash: bool,

    /// Disable autosquash. Last one wins when combined with `--autosquash`.
    /// Alone this is a no-op: non-interactive rebase does not read
    /// `rebase.autosquash`.
    #[clap(
        long = "no-autosquash",
        overrides_with = "autosquash",
        conflicts_with_all = ["continue_rebase", "abort", "skip"]
    )]
    pub no_autosquash: bool,

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

    /// Auto-stage rerere-replayed resolutions for this rebase, overriding
    /// `rerere.autoUpdate`. The last rerere toggle wins.
    #[clap(long = "rerere-autoupdate", overrides_with = "no_rerere_autoupdate")]
    pub rerere_autoupdate: bool,

    /// Do not auto-stage rerere-replayed resolutions for this rebase, overriding
    /// `rerere.autoUpdate`. The last rerere toggle wins.
    #[clap(long = "no-rerere-autoupdate", overrides_with = "rerere_autoupdate")]
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

    /// Interactive rebase: generate a todo, run the sequence editor, and
    /// replay the resulting commands.
    #[clap(short = 'i', long = "interactive")]
    pub interactive: bool,

    /// Rewrite remaining commands of an in-progress interactive rebase.
    #[clap(
        long = "edit-todo",
        conflicts_with_all = ["continue_rebase", "abort", "skip"]
    )]
    pub edit_todo: bool,
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
    #[error("failed to update HEAD during rebase: {0}")]
    HeadUpdate(String),
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
    /// A replay input carries a gitlink the rebase would have to arbitrate —
    /// refused before the first write (ADR-MG-01).
    #[error("{0}")]
    GitlinkUnsupported(String),
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
    #[error("failed to resolve the identity for the replayed commit: {0}")]
    IdentityMissing(String),
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
    #[error("pre-rebase hook failed: {0}")]
    RepositoryHook(String),
    /// Interactive todo is halted on an invalid line (HF-28 / I9).
    #[error("{0}")]
    InteractiveTodoHalted(String),
    /// Interactive `exec` failed (HF-23 / I12). Remaining commands wait for `--continue`.
    #[error("execution failed: {command}{detail}")]
    InteractiveExecFailed { command: String, detail: String },
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
            // §C.13: a HEAD write refused because the branch is checked out
            // in another worktree is a CONFLICT; anything else that fails
            // here is a write fault. The classification comes from the
            // storage layer's own predicate, not from a message match at
            // this boundary.
            RebaseError::HeadUpdate(..) => {
                crate::internal::branch::checked_out_elsewhere_cli_error(&error).unwrap_or_else(
                    || {
                        CliError::fatal(error.to_string())
                            .with_stable_code(StableErrorCode::IoWriteFailed)
                    },
                )
            }
            RebaseError::UpstreamResolve { .. }
            | RebaseError::OntoResolve { .. }
            | RebaseError::NoCommonAncestor => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget),
            RebaseError::GitlinkUnsupported(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::Unsupported)
                .with_hint(
                    "submodule merging is a permanent non-goal; resolve the submodule pointer outside Libra",
                )
                .with_hint(
                    "or drop the gitlink entry from the commits being replayed so no submodule decision is needed",
                ),
            RebaseError::InvalidExec(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("pass a non-empty shell command without NUL bytes"),
            RebaseError::RepositoryHook(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("set LIBRA_NO_HOOKS=1 to bypass repository hooks"),
            RebaseError::InteractiveTodoHalted(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("fix the todo with 'libra rebase --edit-todo' then '--continue'")
                .with_hint("or run 'libra rebase --abort' to return to the original branch"),
            RebaseError::InteractiveExecFailed { command, .. } => {
                CliError::failure(error.to_string())
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint("fix the command or repository state, then run 'libra rebase --continue'")
                    .with_hint("or run 'libra rebase --abort' to return to the original branch")
                    .with_detail("command", command.clone())
            }
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
            } => {
                let mut cli = CliError::fatal(error.to_string())
                    .with_stable_code(kind.stable_code())
                    .with_detail("commit", commit.clone())
                    .with_detail("subject", subject.clone())
                    .with_detail("kind", kind.as_str())
                    .with_detail("detail", detail.clone());
                cli = cli.with_hint("run 'libra rebase --abort' to return to the original branch.");
                // A missing identity is fixed by configuring one, not by aborting.
                // The hint budget is 2, so this goes in front of the generic abort
                // hint rather than after it, where it would be dropped.
                if matches!(kind, ReplayErrorKind::IdentityMissing) {
                    cli = cli.with_priority_hint(
                        "set 'user.name' and 'user.email' with 'libra config', then run 'libra rebase --continue'",
                    );
                }
                cli
            }
            RebaseError::CommitLoad { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
            }
            // Mirrors `CommitError::IdentityMissing`: a replayed commit needs the
            // same committer identity as any other commit.
            RebaseError::IdentityMissing(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("run 'libra config --global user.name \"Your Name\"' and 'libra config --global user.email \"you@example.com\"'")
                .with_hint("omit '--global' to set the identity only in this repository."),
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
/// Resolved start targets after `--root` remaps the optional positional to
/// `<branch>` (Git 2.54). `--root` plus two positionals is `<upstream>`+
/// `<branch>` and is a usage error.
#[derive(Debug)]
struct RebaseStartSpec {
    root: bool,
    upstream: Option<String>,
    onto: Option<String>,
    branch: Option<String>,
}

fn rebase_start_spec(args: &RebaseArgs) -> Result<RebaseStartSpec, CliError> {
    if args.root {
        if args.upstream.is_some() && args.branch.is_some() {
            return Err(CliError::command_usage(
                "--root cannot be used together with <upstream>",
            ));
        }
        return Ok(RebaseStartSpec {
            root: true,
            upstream: None,
            onto: args.onto.clone(),
            branch: args.branch.clone().or_else(|| args.upstream.clone()),
        });
    }
    Ok(RebaseStartSpec {
        root: false,
        upstream: args.upstream.clone(),
        onto: args.onto.clone(),
        branch: args.branch.clone(),
    })
}

pub async fn execute_safe(args: RebaseArgs, output: &OutputConfig) -> CliResult<()> {
    // Part C W1 (§C.4.2): rebase is now safe in a LINKED worktree — its state
    // row is keyed by `worktree_id` (migration 2026072101), its aux sidecar
    // (exec queue / update-refs plan / rewrites / held autostash) lives in
    // THIS worktree's gitdir, the sequencer mutex probes the scoped row, GC
    // traces every scope's state as reachability roots, and the operation
    // dedup window is per-worktree. Two worktrees can rebase their own
    // branches concurrently without interfering, so the former
    // `ensure_main_worktree` guard is lifted. Branch-ref finish safety is
    // unchanged: update-refs excludes branches checked out in ANY worktree
    // and the finish CAS detects concurrent tip movement.
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Refuse to start a NEW rebase while a cherry-pick sequence is in progress
    // (rebase's own --continue/--abort/--skip operate on rebase state, not
    // cherry-pick, so they are exempt from this guard).
    if !(args.continue_rebase || args.abort || args.skip || args.edit_todo) {
        crate::internal::sequencer::ensure_none_in_progress(
            crate::internal::sequencer::SequenceKind::Rebase,
        )
        .await?;
    }

    // For --continue, --abort, --skip: verify that a rebase is actually in
    // progress before delegating to typed runners.  This ensures
    // a non-zero exit code (128) is returned when there is nothing to do,
    // matching the behaviour of `git rebase --abort` / `--continue` / `--skip`.
    if args.continue_rebase || args.abort || args.skip || args.edit_todo {
        match RebaseState::is_in_progress().await {
            Ok(true) => { /* rebase in progress – proceed */ }
            Ok(false) => {
                let verb = if args.abort {
                    "abort"
                } else if args.skip {
                    "skip"
                } else if args.edit_todo {
                    "edit-todo"
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

    // §C.4.2 / ADR-0714-08: adoption of a legacy `rebase-merge/` directory is
    // an EXPLICIT act, and this is the only place it happens — the user has
    // asked to continue, skip or abort THIS rebase, which is the statement of
    // ownership a read cannot make. Reads above only reported that it exists.
    if args.continue_rebase || args.abort || args.skip || args.edit_todo {
        // A bare repository has no working tree to rebase, and the
        // control-action path does not reach the start-path preflight — so it is
        // rejected HERE, before adoption. Otherwise a bare repo holding legacy
        // state would gain a scoped row and lose its recovery directory on its
        // way to an error.
        crate::command::worktree::reject_bare_repository().await?;
        RebaseState::adopt_legacy_state().await.map_err(|err| {
            CliError::fatal(format!("failed to adopt the legacy rebase state: {err}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    }

    let start_spec = rebase_start_spec(&args)?;
    preflight_rebase(&args, &start_spec).await?;
    // Validate `--empty` before any dispatch (start or sequencer control) so a bad
    // mode fails fast (exit 129) rather than slipping through.
    let empty_mode = resolve_rebase_empty_mode(&args)?;
    if args.abort {
        let result = run_rebase_abort().await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if args.continue_rebase {
        let result = run_rebase_continue(output).await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if args.skip {
        let result = run_rebase_skip(output).await.map_err(CliError::from)?;
        return render_rebase_output(&result, output);
    }
    if args.edit_todo {
        return run_rebase_edit_todo(output).await;
    }
    if args.interactive && args.update_refs {
        return Err(CliError::command_usage(
            "the option '--update-refs' cannot be used with '--interactive'",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(
            "interactive `update-ref` todo lines are deferred (DEFER-02); rebase without `--update-refs`",
        ));
    }
    if args.interactive {
        return run_rebase_interactive_hidden(&start_spec, &args, output).await;
    }
    if start_spec.root || start_spec.upstream.is_some() {
        let hook_upstream = start_spec.upstream.as_deref().unwrap_or("--root");
        run_pre_rebase_hook(hook_upstream, start_spec.branch.as_deref(), output)
            .await
            .map_err(CliError::from)?;
        // ADR-MG-01: refuse a submodule-arbitrating replay before
        // `prepare_rebase_aux` writes the autostash / aux sidecar and resets
        // the working tree.
        preflight_rebase_gitlinks(
            start_spec.upstream.as_deref(),
            start_spec.onto.as_deref(),
            start_spec.branch.as_deref(),
            args.fork_point,
            args.no_keep_empty,
            start_spec.root,
        )
        .await
        .map_err(CliError::from)?;
        prepare_rebase_aux(&args).await.map_err(CliError::from)?;
        // `git rebase --onto <newbase> <upstream> <branch>` form: check out the
        // named branch first (no-op when it is already current), so the rest of
        // the start path rebases it as "the current branch". `--root <branch>`
        // uses the same switch-then-replay path (ADR-HF-13 / M-ROOT R4).
        let start_result = async {
            if let Some(branch) = start_spec.branch.as_deref() {
                switch_to_rebase_branch(branch, output).await?;
            }
            run_rebase_start(
                start_spec.upstream.as_deref(),
                start_spec.onto.as_deref(),
                args.autosquash,
                args.no_keep_empty,
                empty_mode,
                args.fork_point,
                start_spec.root,
                output,
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

/// HF-28: generate / edit / parse a todo, then replay `pick`/`drop`/reorder.
async fn run_rebase_interactive_hidden(
    spec: &RebaseStartSpec,
    args: &RebaseArgs,
    output: &OutputConfig,
) -> CliResult<()> {
    let Some(editor_cmd) = editor::resolve_sequence_editor().await else {
        return Err(CliError::failure("no sequence editor configured")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "set GIT_SEQUENCE_EDITOR, sequence.editor, GIT_EDITOR, core.editor, VISUAL, or EDITOR",
            ));
    };

    let plan = collect_interactive_todo_plan(spec)
        .await
        .map_err(CliError::from)?;
    let text = generate_interactive_todo_text(&plan, args)
        .await
        .map_err(CliError::from)?;

    let path = interactive_todo_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CliError::fatal(format!(
                "failed to create the interactive rebase todo directory '{}': {error}",
                parent.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    let _cleanup = InteractiveTodoBufferGuard { path: path.clone() };

    let edited = match editor::edit_message(&path, &text, &editor_cmd, true).await {
        Ok(body) => body,
        Err(error) => return Err(editor_error_to_cli(error)),
    };
    let parsed = match rebase_todo::parse_todo(&edited) {
        Ok(instructions) => instructions,
        Err(error) => {
            persist_interactive_parse_failure(&plan, spec, &error, &edited)
                .await
                .map_err(CliError::from)?;
            return Err(todo_parse_error_to_cli(error));
        }
    };
    if parsed.is_empty() {
        return Err(
            CliError::failure("nothing to do").with_stable_code(StableErrorCode::RepoStateInvalid)
        );
    }

    let picks = match interactive_replay_items(&parsed, &plan.commits) {
        Ok(picks) => picks,
        Err(error) => return Err(interactive_replay_error_to_cli(error)),
    };

    let hook_upstream = spec.upstream.as_deref().unwrap_or("--root");
    run_pre_rebase_hook(hook_upstream, spec.branch.as_deref(), output)
        .await
        .map_err(CliError::from)?;
    preflight_rebase_gitlinks(
        spec.upstream.as_deref(),
        spec.onto.as_deref(),
        spec.branch.as_deref(),
        args.fork_point,
        args.no_keep_empty,
        spec.root,
    )
    .await
    .map_err(CliError::from)?;

    prepare_interactive_start_aux(args)
        .await
        .map_err(CliError::from)?;
    persist_interactive_aux(parsed, None, None, Some(interactive_known_from_plan(&plan)))
        .map_err(CliError::from)?;
    let _picks = picks;

    let start_result = async {
        if let Some(branch) = spec.branch.as_deref() {
            switch_to_rebase_branch(branch, output).await?;
        }
        run_interactive_replay(&plan, spec, output)
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
    render_rebase_output(&result, output)
}

async fn run_rebase_edit_todo(output: &OutputConfig) -> CliResult<()> {
    ensure_rebase_in_progress().await.map_err(CliError::from)?;
    let mut state = RebaseState::load()
        .await
        .map_err(|error| CliError::from(RebaseError::StateLoad(error)))?;
    let Some(aux) = RebaseAuxState::load_optional().map_err(CliError::from)? else {
        return Err(CliError::failure(
            "The --edit-todo action can only be used during an interactive rebase",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    };
    if !aux.marks_interactive() {
        return Err(CliError::failure(
            "The --edit-todo action can only be used during an interactive rebase",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }

    let Some(editor_cmd) = editor::resolve_sequence_editor().await else {
        return Err(CliError::failure("no sequence editor configured")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "set GIT_SEQUENCE_EDITOR, sequence.editor, GIT_EDITOR, core.editor, VISUAL, or EDITOR",
            ));
    };

    let onto_abbrev = short_object_id(&state.onto);
    let head_abbrev = short_object_id(&state.orig_head);
    let text = if let Some(raw) = aux.interactive_todo_text.as_deref() {
        if raw.contains("You are editing the todo file of an ongoing interactive rebase") {
            raw.to_string()
        } else {
            format!(
                "{}{}{}",
                raw.trim_end(),
                if raw.ends_with('\n') { "" } else { "\n" },
                rebase_todo::ONGOING_REBASE_TODO_HINT
            )
        }
    } else {
        rebase_todo::render_remaining_todo(&aux.todo_instructions, &onto_abbrev, &head_abbrev)
    };

    let path = interactive_todo_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CliError::fatal(format!(
                "failed to create the interactive rebase todo directory '{}': {error}",
                parent.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    let _cleanup = InteractiveTodoBufferGuard { path: path.clone() };
    let edited = match editor::edit_message(&path, &text, &editor_cmd, true).await {
        Ok(body) => body,
        Err(error) => return Err(editor_error_to_cli(error)),
    };
    match rebase_todo::parse_todo(&edited) {
        Ok(instructions) => {
            persist_interactive_aux(instructions, None, None, None).map_err(CliError::from)?;
            clear_interactive_stop().map_err(CliError::from)?;
            if state.stopped_sha.is_some() {
                restore_current_head_tree(&state).map_err(CliError::from)?;
            }
            state.todo.clear();
            state.todo_actions.clear();
            state.stopped_sha = None;
            state
                .save()
                .await
                .map_err(|error| CliError::from(RebaseError::StateSave(error)))?;
            if !output.quiet && !output.is_json() {
                println!("Rewrote the remaining interactive rebase todo.");
            }
            Ok(())
        }
        Err(error) => {
            persist_interactive_aux(Vec::new(), Some(error.to_string()), Some(edited), None)
                .map_err(CliError::from)?;
            Err(todo_parse_error_to_cli(error))
        }
    }
}

fn restore_current_head_tree(state: &RebaseState) -> Result<(), RebaseError> {
    let current_commit: Commit =
        load_object(&state.current_head).map_err(|error| RebaseError::CommitLoad {
            commit: state.current_head.to_string(),
            detail: error.to_string(),
        })?;
    let current_tree: Tree =
        load_object(&current_commit.tree_id).map_err(|error| RebaseError::OriginalTreeLoad {
            tree: current_commit.tree_id.to_string(),
            detail: error.to_string(),
        })?;
    let index_file = path::index();
    let current_index = git_internal::internal::index::Index::load(&index_file)
        .map_err(|error| RebaseError::IndexLoad(error.to_string()))?;
    let mut index = git_internal::internal::index::Index::new();
    rebuild_index_from_tree(&current_tree, &mut index, "")
        .map_err(|error| RebaseError::IndexRebuild(error.to_string()))?;
    crate::utils::index_ext::preserve_skip_worktree_from(&current_index, &mut index);
    index
        .save(&index_file)
        .map_err(|error| RebaseError::IndexSave(error.to_string()))?;
    reset_workdir_tracked_only(&current_index, &index)
        .map_err(|error| RebaseError::WorkdirReset(error.to_string()))
}

fn editor_error_to_cli(error: editor::EditorError) -> CliError {
    match error {
        editor::EditorError::WriteBuffer { .. } => {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
        }
        editor::EditorError::ReadBuffer { .. } => {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
        }
        editor::EditorError::Aborted { .. } => CliError::failure(error.to_string())
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("the sequence editor exited without saving a todo list"),
    }
}

fn interactive_todo_path() -> PathBuf {
    util::request_worktree_gitdir_strict()
        .join("rebase-merge")
        .join("git-rebase-todo")
}

struct InteractiveTodoBufferGuard {
    path: PathBuf,
}

impl Drop for InteractiveTodoBufferGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

struct InteractiveTodoPlan {
    onto_id: ObjectHash,
    head_id: ObjectHash,
    onto_abbrev: String,
    head_abbrev: String,
    picks: Vec<rebase_todo::TodoRenderCommit>,
    commits: Vec<ObjectHash>,
}

const INTERACTIVE_TODO_MARKER: &str = "interactive";

fn rebase_aux_is_interactive() -> bool {
    RebaseAuxState::load_optional()
        .ok()
        .flatten()
        .is_some_and(|aux| aux.marks_interactive())
}

fn encode_todo_actions_blob(body: String, interactive: bool) -> String {
    if !interactive {
        return body;
    }
    if body.is_empty() {
        INTERACTIVE_TODO_MARKER.to_string()
    } else {
        format!("{INTERACTIVE_TODO_MARKER}\n{body}")
    }
}

fn decode_todo_actions_blob(content: &str) -> (bool, Vec<&str>) {
    let mut tokens: Vec<_> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let interactive = tokens
        .first()
        .is_some_and(|token| *token == INTERACTIVE_TODO_MARKER);
    if interactive {
        tokens.remove(0);
    }
    (interactive, tokens)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InteractiveReplayError {
    Unresolved(String),
    Ambiguous(String),
    LeadingFold(&'static str),
}

fn interactive_replay_items(
    instructions: &[rebase_todo::TodoInstruction],
    known: &[ObjectHash],
) -> Result<Vec<(ObjectHash, RebaseTodoAction)>, InteractiveReplayError> {
    let mut out = Vec::new();
    for instruction in instructions {
        match instruction {
            rebase_todo::TodoInstruction::Pick { commit } => {
                out.push((resolve_todo_commit(commit, known)?, RebaseTodoAction::Pick));
            }
            rebase_todo::TodoInstruction::Reword { commit } => {
                out.push((
                    resolve_todo_commit(commit, known)?,
                    RebaseTodoAction::Reword,
                ));
            }
            rebase_todo::TodoInstruction::Squash { commit } => {
                out.push((
                    resolve_todo_commit(commit, known)?,
                    RebaseTodoAction::Squash,
                ));
            }
            rebase_todo::TodoInstruction::Fixup { commit, flag } => {
                let action = match flag {
                    None => RebaseTodoAction::Fixup,
                    Some(rebase_todo::FixupFlag::KeepThis) => RebaseTodoAction::FixupKeep,
                    Some(rebase_todo::FixupFlag::Reword) => RebaseTodoAction::FixupKeepEdit,
                };
                out.push((resolve_todo_commit(commit, known)?, action));
            }
            rebase_todo::TodoInstruction::Drop { .. }
            | rebase_todo::TodoInstruction::Exec { .. }
            | rebase_todo::TodoInstruction::Break => {}
            rebase_todo::TodoInstruction::Edit { commit } => {
                out.push((resolve_todo_commit(commit, known)?, RebaseTodoAction::Edit));
            }
        }
    }
    if let Some((_, action)) = out.first()
        && action.folds_into_previous()
    {
        let op = match action {
            RebaseTodoAction::Squash => "squash",
            _ => "fixup",
        };
        return Err(InteractiveReplayError::LeadingFold(op));
    }
    Ok(out)
}

fn resolve_todo_commit(
    abbrev: &str,
    known: &[ObjectHash],
) -> Result<ObjectHash, InteractiveReplayError> {
    let needle = abbrev.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(InteractiveReplayError::Unresolved(abbrev.to_string()));
    }
    let mut found = None;
    for id in known {
        let hex = id.to_string();
        if hex == needle || hex.starts_with(&needle) {
            if found.is_some_and(|existing| existing != *id) {
                return Err(InteractiveReplayError::Ambiguous(abbrev.to_string()));
            }
            found = Some(*id);
        }
    }
    found.ok_or_else(|| InteractiveReplayError::Unresolved(abbrev.to_string()))
}

fn todo_parse_error_to_cli(error: rebase_todo::TodoParseError) -> CliError {
    match &error {
        rebase_todo::TodoParseError::InvalidCommand {
            command,
            number,
            line,
        } => CliError::failure(format!("invalid command '{command}'"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(format!("invalid line {number}: {line}"))
            .with_hint(
                "fix the todo with 'libra rebase --edit-todo' then '--continue', or run 'libra rebase --abort'",
            ),
        rebase_todo::TodoParseError::InvalidLine { number, line } => {
            CliError::failure(format!("invalid line {number}: {line}"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("fix the todo with 'libra rebase --edit-todo' then '--continue'")
                .with_hint("or run 'libra rebase --abort' to return to the original branch")
        }
    }
}

fn interactive_replay_error_to_cli(error: InteractiveReplayError) -> CliError {
    match error {
        InteractiveReplayError::Unresolved(commit) => CliError::failure(format!(
            "could not resolve '{commit}' in the interactive todo"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("HEAD, the index, refs, and sequencer state were left unchanged"),
        InteractiveReplayError::Ambiguous(commit) => CliError::failure(format!(
            "commit '{commit}' is ambiguous in the interactive todo"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("HEAD, the index, refs, and sequencer state were left unchanged"),
        InteractiveReplayError::LeadingFold(op) => {
            CliError::failure(format!("cannot '{op}' without a previous commit"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("HEAD, the index, refs, and sequencer state were left unchanged")
        }
    }
}

async fn edit_interactive_commit_message(commit_id: ObjectHash) -> Result<ObjectHash, RebaseError> {
    let commit: Commit = load_object(&commit_id).map_err(|error| RebaseError::CommitLoad {
        commit: commit_id.to_string(),
        detail: error.to_string(),
    })?;
    let Some(editor_cmd) = editor::resolve_editor().await else {
        return Ok(commit_id);
    };
    let path = util::request_worktree_gitdir_strict().join("COMMIT_EDITMSG");
    let (clean, _) = parse_commit_msg(&commit.message);
    let edited = match editor::edit_message(&path, clean, &editor_cmd, true).await {
        Ok(body) => body,
        Err(editor::EditorError::Aborted { .. }) => {
            return Err(RebaseError::InteractiveTodoHalted(
                "commit message editor exited without saving".to_string(),
            ));
        }
        Err(error) => return Err(RebaseError::Finalize(error.to_string())),
    };
    let trimmed = edited.trim();
    if trimmed.is_empty() {
        return Err(RebaseError::InteractiveTodoHalted(
            "empty commit message after reword".to_string(),
        ));
    }
    if trimmed == clean.trim() {
        return Ok(commit_id);
    }
    let (committer, _) = crate::command::commit::create_committer_signature()
        .await
        .map_err(|error| RebaseError::IdentityMissing(error.to_string()))?;
    let new_commit = Commit::new(
        commit.author.clone(),
        committer,
        commit.tree_id,
        commit.parent_commit_ids.clone(),
        trimmed,
    );
    save_object(&new_commit, &new_commit.id)
        .map_err(|error| RebaseError::CommitSave(error.to_string()))?;
    Ok(new_commit.id)
}

fn persist_interactive_aux(
    todo_instructions: Vec<rebase_todo::TodoInstruction>,
    parse_error: Option<String>,
    todo_text: Option<String>,
    known: Option<Vec<String>>,
) -> Result<(), RebaseError> {
    let mut aux = RebaseAuxState::load_optional()?.unwrap_or_default();
    aux.todo_instructions = todo_instructions;
    aux.interactive_parse_error = parse_error;
    aux.interactive_todo_text = todo_text;
    if let Some(known) = known {
        aux.interactive_known = known;
    }
    aux.save()
}

fn interactive_known_from_plan(plan: &InteractiveTodoPlan) -> Vec<String> {
    plan.commits.iter().map(ToString::to_string).collect()
}

async fn interactive_autosquash_enabled(args: &RebaseArgs) -> Result<bool, RebaseError> {
    if args.no_autosquash {
        return Ok(false);
    }
    if args.autosquash {
        return Ok(true);
    }
    match crate::internal::config::read_cascaded_config_value_strict(
        crate::internal::config::LocalIdentityTarget::CurrentRepo,
        "rebase.autosquash",
    )
    .await
    {
        Ok(None) => Ok(false),
        Ok(Some(value)) => {
            crate::internal::config::parse_git_config_bool(&value).ok_or_else(|| {
                RebaseError::StateLoad(format!("invalid rebase.autosquash value '{value}'"))
            })
        }
        Err(error) => Err(RebaseError::StateLoad(error.to_string())),
    }
}

fn generated_todo_action_token(action: RebaseTodoAction) -> &'static str {
    match action {
        RebaseTodoAction::Squash => "squash",
        RebaseTodoAction::Fixup => "fixup",
        RebaseTodoAction::FixupKeep | RebaseTodoAction::Amend => "fixup -C",
        RebaseTodoAction::FixupKeepEdit => "fixup -c",
        RebaseTodoAction::Pick | RebaseTodoAction::Reword | RebaseTodoAction::Edit => "pick",
    }
}

async fn generate_interactive_todo_text(
    plan: &InteractiveTodoPlan,
    args: &RebaseArgs,
) -> Result<String, RebaseError> {
    let autosquash = interactive_autosquash_enabled(args).await?;
    if !autosquash && args.exec.is_empty() {
        return Ok(rebase_todo::render_todo(
            &plan.picks,
            &plan.onto_abbrev,
            &plan.head_abbrev,
        ));
    }
    let items = if autosquash {
        autosquash_commits(plan.commits.clone())?
    } else {
        plan.commits
            .iter()
            .copied()
            .map(|commit| RebaseTodoItem {
                commit,
                action: RebaseTodoAction::Pick,
            })
            .collect()
    };
    let mut commands = Vec::new();
    for item in items {
        commands.push(format!(
            "{} {} # {}",
            generated_todo_action_token(item.action),
            short_object_id(&item.commit),
            commit_subject_lossy(&item.commit, false)
        ));
        for command in &args.exec {
            commands.push(format!("exec {command}"));
        }
    }
    Ok(rebase_todo::render_todo_with_commands(
        &commands,
        &plan.onto_abbrev,
        &plan.head_abbrev,
    ))
}

async fn prepare_interactive_start_aux(args: &RebaseArgs) -> Result<(), RebaseError> {
    recover_stale_rebase_aux().await?;
    let mut aux = RebaseAuxState {
        rerere_autoupdate: rerere_autoupdate_override(args),
        ..Default::default()
    };
    if args.autostash {
        match crate::command::stash::create_held_stash_commit("autostash").await {
            Ok(Some(stash)) => {
                aux.autostash = Some(stash.to_string());
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
    aux.save()
}

async fn persist_interactive_parse_failure(
    plan: &InteractiveTodoPlan,
    spec: &RebaseStartSpec,
    error: &rebase_todo::TodoParseError,
    edited: &str,
) -> Result<(), RebaseError> {
    persist_interactive_aux(
        Vec::new(),
        Some(error.to_string()),
        Some(edited.to_string()),
        Some(interactive_known_from_plan(plan)),
    )?;
    claim_interactive_onto(plan, spec, Vec::new()).await?;
    Ok(())
}

async fn run_interactive_replay(
    plan: &InteractiveTodoPlan,
    spec: &RebaseStartSpec,
    output: &OutputConfig,
) -> Result<RebaseOutput, RebaseError> {
    let mut state = claim_interactive_onto(plan, spec, Vec::new()).await?;
    let landing_display = spec
        .onto
        .as_deref()
        .or(spec.upstream.as_deref())
        .unwrap_or(plan.onto_abbrev.as_str());
    let branch_name = state.head_name.clone();
    let replay_count = interactive_remaining_count();
    let outcome = drive_interactive(&mut state, &branch_name, landing_display, output).await?;
    Ok(interactive_drive_output(
        "start",
        spec.upstream.clone(),
        Some(plan.onto_id.to_string()),
        Some(replay_count),
        Some(plan.head_id.to_string()),
        branch_name,
        &state,
        outcome,
    ))
}

#[derive(Debug)]
enum InteractiveDriveOutcome {
    Completed(RebaseReplaySummary),
    Stopped {
        summary: RebaseReplaySummary,
        kind: InteractiveStopKind,
    },
}

#[derive(Debug, Clone)]
enum InteractiveStopKind {
    Edit { abbrev: String, subject: String },
    Break { abbrev: String, subject: String },
}

fn format_stopped_at_edit(abbrev: &str, subject: &str) -> String {
    format!(
        "Stopped at {abbrev}...  {subject}\n\
You can amend the commit now with\n\
\n\
\tlibra commit --amend\n\
\n\
Once you are satisfied with your changes, run\n\
\n\
\tlibra rebase --continue"
    )
}

fn format_stopped_at_break(abbrev: &str, subject: &str) -> String {
    format!("Stopped at {abbrev} ({subject})")
}

fn print_interactive_stop(kind: &InteractiveStopKind) {
    match kind {
        InteractiveStopKind::Edit { abbrev, subject } => {
            eprintln!("{}", format_stopped_at_edit(abbrev, subject));
        }
        InteractiveStopKind::Break { abbrev, subject } => {
            eprintln!("{}", format_stopped_at_break(abbrev, subject));
        }
    }
}

fn interactive_remaining_count() -> usize {
    RebaseAuxState::load_optional()
        .ok()
        .flatten()
        .map(|aux| aux.todo_instructions.len())
        .unwrap_or(0)
}

fn interactive_has_remaining_work() -> bool {
    RebaseAuxState::load_optional()
        .ok()
        .flatten()
        .is_some_and(|aux| {
            aux.todo_instructions.iter().any(|instruction| {
                !matches!(instruction, rebase_todo::TodoInstruction::Drop { .. })
            })
        })
}

fn interactive_known_hashes(aux: &RebaseAuxState) -> Vec<ObjectHash> {
    aux.interactive_known
        .iter()
        .filter_map(|oid| ObjectHash::from_str(oid).ok())
        .collect()
}

fn consume_leading_interactive_drops() -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    let mut changed = false;
    while matches!(
        aux.todo_instructions.first(),
        Some(rebase_todo::TodoInstruction::Drop { .. })
    ) {
        let dropped = aux.todo_instructions.remove(0);
        aux.done_instructions.push(dropped);
        changed = true;
    }
    if changed {
        aux.save()?;
    }
    Ok(())
}

fn consume_applied_interactive_instruction() -> Result<(), RebaseError> {
    consume_leading_interactive_drops()?;
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    if aux.todo_instructions.is_empty() {
        return Ok(());
    }
    let done = aux.todo_instructions.remove(0);
    aux.done_instructions.push(done);
    aux.save()
}

fn consume_front_interactive_instruction()
-> Result<Option<rebase_todo::TodoInstruction>, RebaseError> {
    consume_leading_interactive_drops()?;
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(None);
    };
    if aux.todo_instructions.is_empty() {
        return Ok(None);
    }
    let done = aux.todo_instructions.remove(0);
    aux.done_instructions.push(done.clone());
    aux.save()?;
    Ok(Some(done))
}

fn peek_front_interactive_instruction() -> Result<Option<rebase_todo::TodoInstruction>, RebaseError>
{
    consume_leading_interactive_drops()?;
    Ok(RebaseAuxState::load_optional()?.and_then(|aux| aux.todo_instructions.first().cloned()))
}

fn set_interactive_stop(reason: &str) -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    aux.interactive_stop = Some(reason.to_string());
    aux.save()
}

fn clear_interactive_stop() -> Result<(), RebaseError> {
    let Some(mut aux) = RebaseAuxState::load_optional()? else {
        return Ok(());
    };
    aux.interactive_stop = None;
    aux.save()
}

fn stopped_at_from_original(state: &RebaseState) -> (String, String) {
    let id = state.done.last().copied().unwrap_or(state.current_head);
    (short_object_id(&id), commit_subject_lossy(&id, false))
}

fn instruction_to_replay_item(
    instruction: &rebase_todo::TodoInstruction,
    known: &[ObjectHash],
) -> Result<Option<(ObjectHash, RebaseTodoAction)>, InteractiveReplayError> {
    match instruction {
        rebase_todo::TodoInstruction::Pick { commit } => Ok(Some((
            resolve_todo_commit(commit, known)?,
            RebaseTodoAction::Pick,
        ))),
        rebase_todo::TodoInstruction::Reword { commit } => Ok(Some((
            resolve_todo_commit(commit, known)?,
            RebaseTodoAction::Reword,
        ))),
        rebase_todo::TodoInstruction::Edit { commit } => Ok(Some((
            resolve_todo_commit(commit, known)?,
            RebaseTodoAction::Edit,
        ))),
        rebase_todo::TodoInstruction::Squash { commit } => Ok(Some((
            resolve_todo_commit(commit, known)?,
            RebaseTodoAction::Squash,
        ))),
        rebase_todo::TodoInstruction::Fixup { commit, flag } => {
            let action = match flag {
                None => RebaseTodoAction::Fixup,
                Some(rebase_todo::FixupFlag::KeepThis) => RebaseTodoAction::FixupKeep,
                Some(rebase_todo::FixupFlag::Reword) => RebaseTodoAction::FixupKeepEdit,
            };
            Ok(Some((resolve_todo_commit(commit, known)?, action)))
        }
        rebase_todo::TodoInstruction::Drop { .. }
        | rebase_todo::TodoInstruction::Exec { .. }
        | rebase_todo::TodoInstruction::Break => Ok(None),
    }
}

async fn load_next_replay_segment(state: &mut RebaseState) -> Result<bool, RebaseError> {
    consume_leading_interactive_drops()?;
    let Some(aux) = RebaseAuxState::load_optional()? else {
        return Ok(false);
    };
    let known = interactive_known_hashes(&aux);
    let mut items = Vec::new();
    let mut ends_with_edit = false;
    for instruction in &aux.todo_instructions {
        match instruction {
            rebase_todo::TodoInstruction::Drop { .. } => {}
            rebase_todo::TodoInstruction::Break | rebase_todo::TodoInstruction::Exec { .. } => {
                break;
            }
            other => {
                let item = instruction_to_replay_item(other, &known).map_err(|error| {
                    RebaseError::InteractiveTodoHalted(match error {
                        InteractiveReplayError::Unresolved(commit) => {
                            format!("could not resolve '{commit}' in the interactive todo")
                        }
                        InteractiveReplayError::Ambiguous(commit) => {
                            format!("commit '{commit}' is ambiguous in the interactive todo")
                        }
                        InteractiveReplayError::LeadingFold(op) => {
                            format!("cannot '{op}' without a previous commit")
                        }
                    })
                })?;
                if let Some(item) = item {
                    ends_with_edit = item.1 == RebaseTodoAction::Edit;
                    items.push(item);
                    if ends_with_edit {
                        break;
                    }
                }
            }
        }
    }
    if items.is_empty() {
        return Ok(false);
    }
    if state.done.is_empty()
        && items
            .first()
            .is_some_and(|(_, action)| action.folds_into_previous())
    {
        let op = match items[0].1 {
            RebaseTodoAction::Squash => "squash",
            _ => "fixup",
        };
        return Err(RebaseError::InteractiveTodoHalted(format!(
            "cannot '{op}' without a previous commit"
        )));
    }
    let (commits, actions): (Vec<ObjectHash>, Vec<RebaseTodoAction>) = items.into_iter().unzip();
    state.todo = VecDeque::from(commits);
    state.todo_actions = VecDeque::from(actions);
    state.save().await.map_err(RebaseError::StateSave)?;
    Ok(ends_with_edit)
}

async fn run_interactive_exec(state: &mut RebaseState, command: &str) -> Result<(), RebaseError> {
    println!("Executing: {command}");
    let result = run_sandboxed_rebase_exec(command).await.map_err(|detail| {
        RebaseError::InteractiveExecFailed {
            command: command.to_string(),
            detail: format!(": {detail}"),
        }
    })?;
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
        if !result.stdout.ends_with('\n') {
            println!();
        }
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
        if !result.stderr.ends_with('\n') {
            eprintln!();
        }
    }
    if let Some(mut aux) = RebaseAuxState::load_optional()? {
        reconcile_rebase_exec_head(state, &mut aux).await?;
    }
    if result.exit_code != 0 || result.timed_out {
        let detail = if result.timed_out {
            ": command timed out after 900 seconds".to_string()
        } else {
            String::new()
        };
        eprintln!("warning: execution failed: {command}");
        return Err(RebaseError::InteractiveExecFailed {
            command: command.to_string(),
            detail,
        });
    }
    let quiet_output = OutputConfig {
        quiet: true,
        ..Default::default()
    };
    if let Err(error) = switch::ensure_clean_status(&quiet_output).await {
        eprintln!("warning: execution failed: {command}");
        return Err(RebaseError::InteractiveExecFailed {
            command: command.to_string(),
            detail: format!(": command left tracked changes: {error}"),
        });
    }
    Ok(())
}

async fn drive_interactive(
    state: &mut RebaseState,
    branch_name: &str,
    onto_display: &str,
    output: &OutputConfig,
) -> Result<InteractiveDriveOutcome, RebaseError> {
    let mut summary = RebaseReplaySummary::default();
    loop {
        consume_leading_interactive_drops()?;
        if !state.todo.is_empty() {
            let ends_with_edit = state
                .todo_actions
                .back()
                .copied()
                .is_some_and(|action| action == RebaseTodoAction::Edit);
            let replay = continue_replay(state, branch_name, onto_display, false, output).await?;
            summary.applied_commits.extend(replay.applied_commits);
            summary.dropped_commits.extend(replay.dropped_commits);
            if !RebaseState::is_in_progress()
                .await
                .map_err(RebaseError::StateCheck)?
            {
                return Ok(InteractiveDriveOutcome::Completed(summary));
            }
            if ends_with_edit && state.todo.is_empty() && state.stopped_sha.is_none() {
                let (abbrev, subject) = stopped_at_from_original(state);
                set_interactive_stop("edit")?;
                let kind = InteractiveStopKind::Edit { abbrev, subject };
                print_interactive_stop(&kind);
                return Ok(InteractiveDriveOutcome::Stopped { summary, kind });
            }
            continue;
        }

        match peek_front_interactive_instruction()? {
            None => {
                if RebaseState::is_in_progress()
                    .await
                    .map_err(RebaseError::StateCheck)?
                {
                    finalize_rebase(state, false, output)
                        .await
                        .map_err(|error| RebaseError::Finalize(error.to_string()))?;
                }
                return Ok(InteractiveDriveOutcome::Completed(summary));
            }
            Some(rebase_todo::TodoInstruction::Break) => {
                consume_front_interactive_instruction()?;
                let (abbrev, subject) = stopped_at_from_original(state);
                set_interactive_stop("break")?;
                let kind = InteractiveStopKind::Break { abbrev, subject };
                print_interactive_stop(&kind);
                return Ok(InteractiveDriveOutcome::Stopped { summary, kind });
            }
            Some(rebase_todo::TodoInstruction::Exec { cmd }) => {
                match run_interactive_exec(state, &cmd).await {
                    Ok(()) => {
                        consume_front_interactive_instruction()?;
                    }
                    Err(error) => {
                        consume_front_interactive_instruction()?;
                        set_interactive_stop("exec")?;
                        return Err(error);
                    }
                }
            }
            Some(_) => {
                let _ = load_next_replay_segment(state).await?;
                if state.todo.is_empty() {
                    return Err(RebaseError::InteractiveTodoHalted(
                        "interactive todo has a commit command that could not be scheduled"
                            .to_string(),
                    ));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn interactive_drive_output(
    action: &str,
    upstream: Option<String>,
    onto: Option<String>,
    replay_count: Option<usize>,
    previous_commit: Option<String>,
    branch: String,
    state: &RebaseState,
    outcome: InteractiveDriveOutcome,
) -> RebaseOutput {
    let (status, applied_commits, dropped_commits) = match outcome {
        InteractiveDriveOutcome::Completed(summary) => (
            "completed".to_string(),
            summary.applied_commits,
            summary.dropped_commits,
        ),
        InteractiveDriveOutcome::Stopped { summary, kind } => {
            let status = match kind {
                InteractiveStopKind::Edit { .. } => "stopped-edit",
                InteractiveStopKind::Break { .. } => "stopped-break",
            };
            (
                status.to_string(),
                summary.applied_commits,
                summary.dropped_commits,
            )
        }
    };
    RebaseOutput {
        action: action.to_string(),
        status,
        branch,
        commit: state.current_head.to_string(),
        upstream,
        onto,
        common_ancestor: None,
        replay_count,
        previous_commit,
        restored: None,
        applied_commits,
        dropped_commits,
        skipped_commit: None,
        skipped_subject: None,
        remaining: Some(state.todo.len()),
    }
}

async fn amend_head_from_index_if_staged(state: &mut RebaseState) -> Result<(), RebaseError> {
    let index_file = path::index();
    let index = git_internal::internal::index::Index::load(&index_file)
        .map_err(|error| RebaseError::IndexLoad(error.to_string()))?;
    crate::internal::layer::reject_layer_owned_entries(&index, "to continue the rebase")
        .await
        .map_err(RebaseError::IndexLoad)?;
    if has_unmerged_entries(&index) {
        return Err(RebaseError::UnresolvedConflicts);
    }
    let new_tree_id = create_tree_from_index(&index)
        .map_err(|error| RebaseError::TreeCreate(error.to_string()))?;
    let head_commit: Commit =
        load_object(&state.current_head).map_err(|error| RebaseError::CommitLoad {
            commit: state.current_head.to_string(),
            detail: error.to_string(),
        })?;
    if new_tree_id == head_commit.tree_id {
        return Ok(());
    }
    let (committer, _) = crate::command::commit::create_committer_signature()
        .await
        .map_err(|error| RebaseError::IdentityMissing(error.to_string()))?;
    let (clean, _) = parse_commit_msg(&head_commit.message);
    let new_commit = Commit::new(
        head_commit.author.clone(),
        committer,
        new_tree_id,
        head_commit.parent_commit_ids.clone(),
        clean.trim(),
    );
    save_object(&new_commit, &new_commit.id)
        .map_err(|error| RebaseError::CommitSave(error.to_string()))?;
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;
    Head::update_result_with_conn(&db, Head::Detached(new_commit.id), None)
        .await
        .map_err(|error| RebaseError::HeadUpdate(error.to_string()))?;
    state.current_head = new_commit.id;
    state.save().await.map_err(RebaseError::StateSave)
}

async fn reconcile_interactive_edit_continue(state: &mut RebaseState) -> Result<(), RebaseError> {
    if let Some(actual) = Head::current_commit().await
        && actual != state.current_head
    {
        state.current_head = actual;
        state.save().await.map_err(RebaseError::StateSave)?;
    }
    amend_head_from_index_if_staged(state).await
}

#[allow(clippy::too_many_arguments)]
async fn finish_replay_or_drive(
    state: &mut RebaseState,
    branch: &str,
    onto_display: &str,
    action: &str,
    previous_commit: Option<String>,
    mut applied_commits: Vec<RebaseAppliedCommitOutput>,
    mut dropped_commits: Vec<RebaseDroppedCommitOutput>,
    skipped_commit: Option<String>,
    skipped_subject: Option<String>,
    output: &OutputConfig,
) -> Result<RebaseOutput, RebaseError> {
    if rebase_aux_is_interactive() {
        let outcome = drive_interactive(state, branch, onto_display, output).await?;
        let mut result = interactive_drive_output(
            action,
            None,
            Some(state.onto.to_string()),
            None,
            previous_commit,
            branch.to_string(),
            state,
            outcome,
        );
        result.applied_commits.splice(0..0, applied_commits);
        result.dropped_commits.splice(0..0, dropped_commits);
        result.skipped_commit = skipped_commit;
        result.skipped_subject = skipped_subject;
        return Ok(result);
    }
    if state.todo.is_empty() {
        finalize_rebase(state, false, output)
            .await
            .map_err(|error| RebaseError::Finalize(error.to_string()))?;
    } else {
        state.save().await.map_err(RebaseError::StateSave)?;
        let replay = continue_replay(state, branch, onto_display, false, output).await?;
        applied_commits.extend(replay.applied_commits);
        dropped_commits.extend(replay.dropped_commits);
    }
    Ok(RebaseOutput {
        action: action.to_string(),
        status: "completed".to_string(),
        branch: branch.to_string(),
        commit: state.current_head.to_string(),
        upstream: None,
        onto: Some(state.onto.to_string()),
        common_ancestor: None,
        replay_count: None,
        previous_commit,
        restored: None,
        applied_commits,
        dropped_commits,
        skipped_commit,
        skipped_subject,
        remaining: Some(state.todo.len()),
    })
}

async fn claim_interactive_onto(
    plan: &InteractiveTodoPlan,
    spec: &RebaseStartSpec,
    items: Vec<(ObjectHash, RebaseTodoAction)>,
) -> Result<RebaseState, RebaseError> {
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;

    let current_branch_name = match Head::current().await {
        Head::Branch(name) if !name.is_empty() => name,
        _ => return Err(RebaseError::NotOnBranch),
    };
    let head_to_rebase_id = plan.head_id;
    let newbase_id = plan.onto_id;

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

    let landing_display = spec
        .onto
        .as_deref()
        .or(spec.upstream.as_deref())
        .unwrap_or(plan.onto_abbrev.as_str());
    let start_action = ReflogAction::Rebase {
        state: "start".to_string(),
        details: format!("checkout {landing_display}"),
    };
    let start_context = ReflogContext {
        old_oid: head_to_rebase_id.to_string(),
        new_oid: newbase_id.to_string(),
        action: start_action,
    };
    crate::internal::db::write_transaction(&db, |txn| {
        Box::pin(async move {
            reflog::Reflog::insert_single_entry(txn, &start_context, "HEAD").await?;
            Head::update_result_with_conn(txn, Head::Detached(newbase_id), None)
                .await
                .map_err(|error| ReflogError::from(sea_orm::DbErr::Custom(error.to_string())))?;
            Ok::<_, ReflogError>(())
        })
    })
    .await
    .map_err(|e| RebaseError::Finalize(format!("failed to start rebase: {e}")))?;

    let (commits, actions): (Vec<ObjectHash>, Vec<RebaseTodoAction>) = items.into_iter().unzip();
    let todo_actions = VecDeque::from(actions);
    let state = RebaseState {
        head_name: current_branch_name,
        onto: newbase_id,
        orig_head: head_to_rebase_id,
        todo: VecDeque::from(commits),
        todo_actions,
        done: Vec::new(),
        stopped_sha: None,
        current_head: newbase_id,
        autosquash: false,
        empty_mode: RebaseEmptyMode::Keep,
    };
    state.claim_start().await.map_err(RebaseError::StateSave)?;
    Head::update_result_with_conn(&db, Head::Detached(newbase_id), None)
        .await
        .map_err(|error| RebaseError::HeadUpdate(error.to_string()))?;
    Ok(state)
}

async fn collect_interactive_todo_plan(
    spec: &RebaseStartSpec,
) -> Result<InteractiveTodoPlan, RebaseError> {
    let current_branch_name = match Head::current().await {
        Head::Branch(name) if !name.is_empty() => name,
        _ => return Err(RebaseError::NotOnBranch),
    };
    let head_id = Head::current_commit()
        .await
        .ok_or_else(|| RebaseError::BranchHasNoCommits {
            branch: current_branch_name.clone(),
        })?;

    let (onto_id, commits) =
        if spec.root {
            let commits = collect_commits_from_root(&head_id)
                .await
                .map_err(|detail| RebaseError::CommitLoad {
                    commit: head_id.to_string(),
                    detail,
                })?;
            let root_id = *commits
                .first()
                .ok_or_else(|| RebaseError::BranchHasNoCommits {
                    branch: current_branch_name.clone(),
                })?;
            let onto_id = match spec.onto.as_deref() {
                Some(onto) => resolve_branch_or_commit(onto).await.map_err(|detail| {
                    RebaseError::OntoResolve {
                        onto: onto.to_string(),
                        detail,
                    }
                })?,
                None => root_id,
            };
            (onto_id, commits)
        } else {
            let upstream =
                spec.upstream
                    .as_deref()
                    .ok_or_else(|| RebaseError::UpstreamResolve {
                        upstream: String::new(),
                        detail: "no upstream specified".to_string(),
                    })?;
            let upstream_id = resolve_branch_or_commit(upstream).await.map_err(|detail| {
                RebaseError::UpstreamResolve {
                    upstream: upstream.to_string(),
                    detail,
                }
            })?;
            let onto_id = match spec.onto.as_deref() {
                Some(onto) => resolve_branch_or_commit(onto).await.map_err(|detail| {
                    RebaseError::OntoResolve {
                        onto: onto.to_string(),
                        detail,
                    }
                })?,
                None => upstream_id,
            };
            let base_id = crate::internal::merge_base::merge_base(&head_id, &upstream_id)
                .map_err(|error| RebaseError::CommitLoad {
                    commit: head_id.to_string(),
                    detail: format!("computing merge base with {upstream_id}: {error}"),
                })?
                .ok_or(RebaseError::NoCommonAncestor)?;
            let commits = collect_commits_to_replay(&base_id, &head_id)
                .await
                .map_err(|detail| RebaseError::CommitLoad {
                    commit: head_id.to_string(),
                    detail,
                })?;
            (onto_id, commits)
        };

    let picks = commits
        .iter()
        .map(|id| rebase_todo::TodoRenderCommit {
            abbrev: short_object_id(id),
            subject: commit_subject_lossy(id, false),
        })
        .collect();
    Ok(InteractiveTodoPlan {
        onto_id,
        head_id,
        onto_abbrev: short_object_id(&onto_id),
        head_abbrev: short_object_id(&head_id),
        picks,
        commits,
    })
}

async fn run_pre_rebase_hook(
    upstream: &str,
    branch: Option<&str>,
    output: &OutputConfig,
) -> Result<(), RebaseError> {
    let mut args = vec![upstream.to_string()];
    if let Some(branch) = branch {
        args.push(branch.to_string());
    }
    let Some(hook_output) = run_repo_hook(RepoHook::PreRebase, &args)
        .await
        .map_err(|error| RebaseError::RepositoryHook(error.to_string()))?
    else {
        return Ok(());
    };
    replay_repo_hook_output(&hook_output, output).map_err(RebaseError::RepositoryHook)?;
    if hook_output.timed_out {
        return Err(RebaseError::RepositoryHook(format!(
            "hook '{}' exceeded the 15 minute timeout",
            hook_output.path.display()
        )));
    }
    if hook_output.exit_code != 0 {
        return Err(RebaseError::RepositoryHook(format!(
            "hook '{}' failed with exit code {}",
            hook_output.path.display(),
            hook_output.exit_code
        )));
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
            // Rebase's internal branch switch never requests the bypass flag;
            // the same-branch guard applies unchanged.
            ignore_other_worktrees: false,
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
        "stopped-edit" | "stopped-break" => {}
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
    /// Interactive `reword`: pick, then open the commit-message editor.
    Reword,
    /// Interactive `fixup -C`: fold and keep this commit's message.
    FixupKeep,
    /// Interactive `fixup -c`: fold, keep this commit's message, then edit.
    FixupKeepEdit,
    /// Interactive `edit`: pick, then stop for amend.
    Edit,
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
            "reword" => Ok(Self::Reword),
            "fixup_c" => Ok(Self::FixupKeep),
            "fixup_c_edit" => Ok(Self::FixupKeepEdit),
            "edit" => Ok(Self::Edit),
            other => Err(format!("invalid rebase todo action '{other}'")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Fixup => "fixup",
            Self::Squash => "squash",
            Self::Amend => "amend",
            Self::Reword => "reword",
            Self::FixupKeep => "fixup_c",
            Self::FixupKeepEdit => "fixup_c_edit",
            Self::Edit => "edit",
        }
    }

    fn folds_into_previous(self) -> bool {
        matches!(
            self,
            Self::Fixup | Self::Squash | Self::Amend | Self::FixupKeep | Self::FixupKeepEdit
        )
    }
}

fn replay_genealogy_predecessors(
    original_commit: &Commit,
    previous_commit: ObjectHash,
    action: RebaseTodoAction,
) -> Vec<(String, RelationKind)> {
    match action {
        RebaseTodoAction::Fixup
        | RebaseTodoAction::Squash
        | RebaseTodoAction::FixupKeep
        | RebaseTodoAction::FixupKeepEdit => vec![
            (previous_commit.to_string(), RelationKind::Squash),
            (original_commit.id.to_string(), RelationKind::Squash),
        ],
        RebaseTodoAction::Amend => vec![
            (previous_commit.to_string(), RelationKind::Amend),
            (original_commit.id.to_string(), RelationKind::Amend),
        ],
        RebaseTodoAction::Pick | RebaseTodoAction::Reword | RebaseTodoAction::Edit => {
            vec![(original_commit.id.to_string(), RelationKind::Rebase)]
        }
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

async fn preflight_rebase(args: &RebaseArgs, spec: &RebaseStartSpec) -> CliResult<()> {
    if args.continue_rebase || args.abort || args.skip || args.edit_todo {
        return Ok(());
    }

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
    if let Some(upstream) = spec.upstream.as_deref() {
        resolve_branch_or_commit(upstream)
            .await
            .map_err(CliError::from_legacy_string)?;
    } else if !spec.root {
        return Err(CliError::fatal("no upstream specified"));
    }
    if let Some(branch) = spec.branch.as_deref() {
        resolve_branch_or_commit(branch)
            .await
            .map_err(CliError::from_legacy_string)?;
    }

    // Pre-resolve the --onto target so an unresolvable newbase fails fast,
    // before any worktree/state mutation (run_rebase_start re-resolves it for
    // the typed `OntoResolve` error).
    if let Some(onto) = spec.onto.as_deref() {
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
        rerere_autoupdate: rerere_autoupdate_override(args),
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
    // Rewrites are also the stdin contract for `post-rewrite`; keep the sidecar
    // for every rebase, not only exec/update-refs/autostash runs.
    aux.save()?;
    Ok(())
}

const fn rerere_autoupdate_override(args: &RebaseArgs) -> Option<bool> {
    if args.rerere_autoupdate {
        Some(true)
    } else if args.no_rerere_autoupdate {
        Some(false)
    } else {
        None
    }
}

fn persisted_rerere_autoupdate() -> Result<Option<bool>, RebaseError> {
    Ok(RebaseAuxState::load_optional()?.and_then(|aux| aux.rerere_autoupdate))
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
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;
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

    let cwd = util::request_working_dir();
    let sandbox = ToolSandboxContext {
        // The exec command is explicit user CLI input (not repo content) and
        // may legitimately run nested VCS operations (`libra add`/`commit`),
        // so `.libra` metadata must stay writable; network remains denied.
        policy: SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.clone()],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
            allow_metadata_writes: true,
        },
        permissions: SandboxPermissions::UseDefault,
    };
    let runtime = SandboxRuntimeConfig {
        enforcement: SandboxEnforcement::Required,
        use_linux_sandbox_bwrap: true,
        ..Default::default()
    };
    // Rebase --exec may invoke Libra again inside this operation. Its parent
    // still owns the operation boundary leases, so descendants inherit these
    // markers and avoid waiting on leases held by their own parent.
    let command = format!(
        "export {}=1; export {}=1; {command}",
        crate::internal::operation::middleware::REPOSITORY_REF_LEASE_HELD_ENV,
        crate::internal::operation::middleware::OPERATION_SCOPE_LEASE_HELD_ENV,
    );
    run_shell_command(
        &command,
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
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;
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

/// ADR-MG-01 gate for `rebase`, ahead of every mutation the start path makes —
/// the `--autostash` stash commit + worktree reset, the aux sidecar, the branch
/// switch, the HEAD detach, and the state claim.
///
/// The per-replay guard in the unified tree engine cannot serve
/// here: a step's "ours" side only exists once the previous steps have been
/// applied, so a refusal there arrives after HEAD has already moved. This asks
/// the conservative whole-sequence question instead — every input tree of the
/// replay must record the same pointer for a given gitlink path
/// (`merge::ensure_gitlinks_uniform_across_inputs`).
///
/// Resolution failures are deliberately NOT reported here: `run_rebase_start`
/// owns those error messages, and a preflight that fails first would change
/// them. Only a gitlink refusal escapes.
pub(crate) async fn preflight_gitlinks_for_pull(upstream: &str) -> Result<(), RebaseError> {
    preflight_rebase_gitlinks(Some(upstream), None, None, false, false, false).await
}

async fn preflight_rebase_gitlinks(
    upstream: Option<&str>,
    onto: Option<&str>,
    branch: Option<&str>,
    fork_point: bool,
    no_keep_empty: bool,
    root: bool,
) -> Result<(), RebaseError> {
    let head_id = match branch {
        // `rebase --onto <newbase> <upstream> <branch>` checks `<branch>` out
        // first, so THAT is the branch whose commits will be replayed.
        Some(branch) => match resolve_branch_or_commit(branch).await {
            Ok(id) => id,
            Err(_) => return Ok(()),
        },
        None => match Head::current_commit().await {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let (newbase_id, mut commits) = if root {
        let Ok(commits) = collect_commits_from_root(&head_id).await else {
            return Ok(());
        };
        let Some(root_id) = commits.first().copied() else {
            return Ok(());
        };
        let newbase_id = match onto {
            Some(target) => match resolve_branch_or_commit(target).await {
                Ok(id) => id,
                Err(_) => return Ok(()),
            },
            None => root_id,
        };
        (newbase_id, commits)
    } else {
        let Some(upstream) = upstream else {
            return Ok(());
        };
        let Ok(upstream_id) = resolve_branch_or_commit(upstream).await else {
            return Ok(());
        };
        let newbase_id = match onto {
            Some(target) => match resolve_branch_or_commit(target).await {
                Ok(id) => id,
                Err(_) => return Ok(()),
            },
            None => upstream_id,
        };
        let Ok(Some(ordinary_base)) =
            crate::internal::merge_base::merge_base(&head_id, &upstream_id)
        else {
            return Ok(());
        };
        let base_id = if fork_point {
            match reflog_fork_point(upstream, upstream_id, head_id).await {
                Ok(found) => found.unwrap_or(ordinary_base),
                Err(_) => return Ok(()),
            }
        } else {
            ordinary_base
        };
        // Both of `run_rebase_start`'s short-circuits decide nothing, so neither may
        // be pre-empted by a gitlink refusal: `base_id == head_id` fast-forwards
        // onto the upstream tree wholesale, and `base_id == upstream_id` means the
        // upstream is already an ancestor (already up to date) — with no `--onto`
        // there is nothing to move.
        if onto.is_none() && (base_id == head_id || base_id == upstream_id) {
            return Ok(());
        }
        let Ok(commits) = collect_commits_to_replay(&base_id, &head_id).await else {
            return Ok(());
        };
        (newbase_id, commits)
    };
    if no_keep_empty {
        // `--no-keep-empty` prunes already-empty commits from the replay list
        // BEFORE any of them is replayed, so they are not inputs at all. The
        // verdict is normally unchanged — an empty commit's tree equals its
        // first parent's, which stays an input — except when EVERY commit is
        // pruned: then nothing is replayed and nothing may be refused.
        let mut kept = Vec::with_capacity(commits.len());
        for commit_id in commits {
            if !commit_starts_empty(&commit_id).await {
                kept.push(commit_id);
            }
        }
        commits = kept;
    }
    if commits.is_empty() {
        return Ok(());
    }

    // The inputs of the actual replay: the landing tree the first step merges
    // onto, and — for each replayed commit — its own tree plus every original
    // parent. The shared tree engine folds multiple parent bases into its
    // recursive virtual ancestor before applying the flattened replay.
    let mut inputs = Vec::with_capacity(1 + commits.len() * 2);
    match commit_gitlinks(&newbase_id) {
        Ok(gitlinks) => inputs.push(gitlinks),
        Err(_) => return Ok(()),
    }
    for commit_id in &commits {
        let Ok(commit) = load_object::<Commit>(commit_id) else {
            return Ok(());
        };
        match commit_gitlinks(commit_id) {
            Ok(gitlinks) => inputs.push(gitlinks),
            Err(_) => return Ok(()),
        }
        if commit.parent_commit_ids.is_empty() {
            // `--root` replays a parentless commit against an empty base; there
            // is no parent gitlink to include.
            continue;
        }
        for parent_id in &commit.parent_commit_ids {
            match commit_gitlinks(parent_id) {
                Ok(gitlinks) => inputs.push(gitlinks),
                Err(_) => return Ok(()),
            }
        }
    }
    merge::ensure_gitlinks_uniform_across_inputs("rebase", &inputs)
        .map_err(|refusal| RebaseError::GitlinkUnsupported(refusal.to_string()))
}

/// The gitlink entries of `commit_id`'s tree, for the replay preflight.
fn commit_gitlinks(commit_id: &ObjectHash) -> Result<merge::GitlinkEntries, String> {
    let commit: Commit = load_object(commit_id).map_err(|error| error.to_string())?;
    merge::commit_gitlink_entries(&commit).map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn run_rebase_start(
    upstream: Option<&str>,
    onto: Option<&str>,
    autosquash: bool,
    no_keep_empty: bool,
    empty_mode: RebaseEmptyMode,
    fork_point: bool,
    root: bool,
    output: &OutputConfig,
) -> Result<RebaseOutput, RebaseError> {
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;

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

    let onto_id = match onto {
        Some(target) => Some(resolve_branch_or_commit(target).await.map_err(|detail| {
            RebaseError::OntoResolve {
                onto: target.to_string(),
                detail,
            }
        })?),
        None => None,
    };

    let (newbase_id, base_id, upstream_id, mut commits_to_replay, upstream_label) = if root {
        let commits = collect_commits_from_root(&head_to_rebase_id)
            .await
            .map_err(|detail| RebaseError::CommitLoad {
                commit: head_to_rebase_id.to_string(),
                detail,
            })?;
        let root_id = *commits
            .first()
            .ok_or_else(|| RebaseError::BranchHasNoCommits {
                branch: current_branch_name.clone(),
            })?;
        let newbase_id = onto_id.unwrap_or(root_id);
        (newbase_id, root_id, root_id, commits, "--root".to_string())
    } else {
        let upstream = upstream.ok_or_else(|| RebaseError::UpstreamResolve {
            upstream: String::new(),
            detail: "no upstream specified".to_string(),
        })?;
        let upstream_id = resolve_branch_or_commit(upstream).await.map_err(|detail| {
            RebaseError::UpstreamResolve {
                upstream: upstream.to_string(),
                detail,
            }
        })?;
        let newbase_id = onto_id.unwrap_or(upstream_id);
        let merge_base_result =
            crate::internal::merge_base::merge_base(&head_to_rebase_id, &upstream_id).map_err(
                |error| RebaseError::CommitLoad {
                    commit: head_to_rebase_id.to_string(),
                    detail: format!("computing merge base with {upstream_id}: {error}"),
                },
            )?;
        match merge_base_result {
            Some(ordinary_base) => {
                let base_id = if fork_point {
                    reflog_fork_point(upstream, upstream_id, head_to_rebase_id)
                        .await?
                        .unwrap_or(ordinary_base)
                } else {
                    ordinary_base
                };
                (
                    newbase_id,
                    base_id,
                    upstream_id,
                    Vec::new(),
                    upstream.to_string(),
                )
            }
            // No common ancestor (ADR-HP-08): replay the whole history from the
            // root commit onto the upstream, equivalent to `rebase --root --onto
            // <upstream>` (Git shows `unrelated, ...` in the replay log).
            None => {
                let commits = collect_commits_from_root(&head_to_rebase_id)
                    .await
                    .map_err(|detail| RebaseError::CommitLoad {
                        commit: head_to_rebase_id.to_string(),
                        detail,
                    })?;
                let root_id = *commits
                    .first()
                    .ok_or_else(|| RebaseError::BranchHasNoCommits {
                        branch: current_branch_name.clone(),
                    })?;
                (
                    newbase_id,
                    root_id,
                    upstream_id,
                    commits,
                    upstream.to_string(),
                )
            }
        }
    };

    // Fast-forward and already-up-to-date short-circuits apply only to a plain
    // rebase (no explicit --onto), and only when the replay list was not already
    // populated from an unrelated-history root replay (ADR-HP-08). With --onto,
    // an explicit landing point must always replay <upstream>..HEAD onto <newbase>.
    if !root && onto.is_none() && base_id == head_to_rebase_id && commits_to_replay.is_empty() {
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
        crate::utils::index_ext::preserve_skip_worktree_from(&current_index, &mut index);
        rebase_worktree_guard_structured(&index, "fast-forward rebase").await?;
        // The worktree is materialized AFTER the ref and index move below, so
        // anything that materialization would refuse has to be caught here —
        // otherwise the branch ends up ahead of the working tree. Most visibly:
        // a submodule directory the upstream tree no longer declares
        // (ADR-MG-01), which `restore` refuses to replace when it is non-empty.
        crate::command::restore::preflight_worktree_restore_to_commit(&upstream_id)
            .await
            .map_err(|error| RebaseError::WorktreeStatus(error.to_string()))?;

        let fast_forward_action = ReflogAction::Rebase {
            state: "fast-forward".to_string(),
            details: format!("moving {} to {}", current_branch_name, upstream_label),
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
                    Head::update_result_with_conn(txn, Head::Branch(branch_name_cloned), None)
                        .await
                        .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
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
            upstream: Some(upstream_label.clone()),
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

    // Explicit `--autosquash` must still replay (and fold) when upstream is an
    // ancestor of HEAD. Without the flag, keep Git's already-up-to-date shortcut.
    // `--root` and an unrelated-history replay never take this shortcut.
    if !root
        && onto.is_none()
        && base_id == upstream_id
        && !autosquash
        && commits_to_replay.is_empty()
    {
        return Ok(RebaseOutput {
            action: "start".to_string(),
            status: "already-up-to-date".to_string(),
            branch: current_branch_name,
            commit: head_to_rebase_id.to_string(),
            upstream: Some(upstream_label.clone()),
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

    if !root && commits_to_replay.is_empty() {
        commits_to_replay = collect_commits_to_replay(&base_id, &head_to_rebase_id)
            .await
            .map_err(|detail| RebaseError::CommitLoad {
                commit: head_to_rebase_id.to_string(),
                detail,
            })?;
    }
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
            upstream: Some(upstream_label.clone()),
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
    let landing_display = onto.unwrap_or(upstream_label.as_str());
    let start_action = ReflogAction::Rebase {
        state: "start".to_string(),
        details: format!("checkout {}", landing_display),
    };
    let start_context = ReflogContext {
        old_oid: head_to_rebase_id.to_string(),
        new_oid: newbase_id.to_string(),
        action: start_action,
    };
    // The reflog insert reads `user.name`/`user.email` before either write,
    // so this transaction must take the write lock up front — otherwise a
    // second worktree rebasing at the same moment makes it fail rather than
    // wait (`db::begin_write_transaction`).
    crate::internal::db::write_transaction(&db, |txn| {
        Box::pin(async move {
            reflog::Reflog::insert_single_entry(txn, &start_context, "HEAD").await?;
            Head::update_result_with_conn(txn, Head::Detached(newbase_id), None)
                .await
                .map_err(|error| ReflogError::from(sea_orm::DbErr::Custom(error.to_string())))?;
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

    // The first write of a STARTING rebase is a claim, not a replace (§C.4.4).
    state.claim_start().await.map_err(RebaseError::StateSave)?;
    Head::update_result_with_conn(&db, Head::Detached(newbase_id), None)
        .await
        .map_err(|error| RebaseError::HeadUpdate(error.to_string()))?;

    let replay = continue_replay(
        &mut state,
        &current_branch_name,
        landing_display,
        false,
        output,
    )
    .await?;

    Ok(RebaseOutput {
        action: "start".to_string(),
        status: "completed".to_string(),
        branch: current_branch_name,
        commit: state.current_head.to_string(),
        upstream: Some(upstream_label),
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
pub(crate) async fn run_rebase_for_pull(
    upstream: &str,
    output: &OutputConfig,
) -> Result<PullRebaseSummary, RebaseError> {
    run_pre_rebase_hook(upstream, None, output).await?;
    // ADR-MG-01: `pull --rebase` is a second entry into the replay. `pull` calls
    // `preflight_gitlinks_for_pull` BEFORE its own autostash push, so the gate
    // has already run by the time we get here; repeating it costs one cheap
    // history walk and keeps this entry correct on its own.
    preflight_rebase_gitlinks(Some(upstream), None, None, false, false, false).await?;
    // `pull --rebase` keeps Libra's default (keep become-empty commits).
    let output = run_rebase_start(
        Some(upstream),
        None,
        false,
        false,
        RebaseEmptyMode::Keep,
        false,
        false,
        output,
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
    output: &OutputConfig,
) -> Result<RebaseReplaySummary, RebaseError> {
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(RebaseError::StateSave)?;
    let rerere_autoupdate = persisted_rerere_autoupdate()?;
    let interactive = rebase_aux_is_interactive();
    let mut squash_needs_editor = false;
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
        match replay_commit_with_unified_merge(
            &commit_id,
            &state.current_head,
            action,
            state.empty_mode,
            rerere_autoupdate,
        )
        .await
        {
            ReplayResult::BecameEmptyDropped { subject } => {
                // `--empty=drop`: the commit became empty after replay; skip it
                // without advancing `current_head` (the new parent is unchanged).
                state.todo.pop_front();
                state.todo_actions.pop_front();
                if interactive {
                    consume_applied_interactive_instruction()?;
                }
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
                if interactive {
                    consume_applied_interactive_instruction()?;
                }

                if interactive {
                    let next_folds = state
                        .todo_actions
                        .front()
                        .is_some_and(|next| next.folds_into_previous());
                    let mut edit_message = matches!(
                        action,
                        RebaseTodoAction::Reword | RebaseTodoAction::FixupKeepEdit
                    );
                    if matches!(action, RebaseTodoAction::Squash) {
                        squash_needs_editor = true;
                    }
                    if squash_needs_editor && !next_folds {
                        edit_message = true;
                        squash_needs_editor = false;
                    }
                    if edit_message {
                        state.current_head =
                            edit_interactive_commit_message(state.current_head).await?;
                    }
                }

                // Update HEAD
                Head::update_result_with_conn(&db, Head::Detached(state.current_head), None)
                    .await
                    .map_err(|error| RebaseError::HeadUpdate(error.to_string()))?;

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
                    state.current_head,
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

    // All commits replayed successfully - finalize unless interactive
    // instructions (exec/break/later picks) are still waiting.
    if interactive {
        consume_leading_interactive_drops()?;
        if interactive_has_remaining_work() {
            return Ok(summary);
        }
    }
    finalize_rebase(state, emit_human, output)
        .await
        .map_err(|e| RebaseError::Finalize(e.to_string()))?;
    Ok(summary)
}

/// Finalize rebase after all commits are replayed
async fn finalize_rebase(
    state: &RebaseState,
    emit_human: bool,
    output: &OutputConfig,
) -> anyhow::Result<()> {
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
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
    crate::utils::index_ext::preserve_skip_worktree_from(&current_index, &mut index);
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
                Head::update_result_with_conn(txn, Head::Branch(branch_name_cloned.clone()), None)
                    .await
                    .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
                Ok(())
            })
        },
        true,
    )
    .await
    {
        // Best-effort recovery path: the caller is already returning the
        // original failure, so a second failure here is logged rather than
        // replacing it — but it is no longer invisible.
        if let Err(error) =
            Head::update_result_with_conn(&db, Head::Detached(final_commit_id), None).await
        {
            tracing::error!(%error, "failed to restore HEAD after a rebase finish failure");
        }
        return Err(e).context("failed to record reflog for rebase finish");
    }

    RebaseState::cleanup()
        .await
        .map_err(anyhow::Error::msg)
        .context("failed to clean up completed rebase state")?;
    resolve_rebase_autostash()
        .await
        .context("failed to restore rebase autostash")?;
    if let Some(aux) = aux.as_ref()
        && !aux.rewrites.is_empty()
    {
        let rewrite_input = aux
            .rewrites
            .iter()
            .map(|(old, new)| format!("{old} {new}\n"))
            .collect::<String>();
        run_advisory_repo_hook(
            RepoHook::PostRewrite,
            &["rebase".to_string()],
            Some(rewrite_input.as_bytes()),
            output,
        )
        .await;
    }
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

async fn run_rebase_continue(output: &OutputConfig) -> Result<RebaseOutput, RebaseError> {
    ensure_rebase_in_progress().await?;
    if let Some(aux) = RebaseAuxState::load_optional()?
        && let Some(detail) = aux.interactive_parse_error
    {
        return Err(RebaseError::InteractiveTodoHalted(detail));
    }
    let mut state = RebaseState::load().await.map_err(RebaseError::StateLoad)?;
    let previous_commit = state.current_head.to_string();
    let branch = state.head_name.clone();
    let onto_display = short_object_id(&state.onto);
    let mut applied_commits = Vec::new();
    let dropped_commits = Vec::new();

    if RebaseAuxState::load_optional()?
        .and_then(|aux| aux.pending_exec)
        .is_some()
    {
        run_pending_rebase_exec(&mut state).await?;
        return finish_replay_or_drive(
            &mut state,
            &branch,
            &onto_display,
            "continue",
            Some(previous_commit),
            applied_commits,
            dropped_commits,
            None,
            None,
            output,
        )
        .await;
    }

    if RebaseAuxState::load_optional()?
        .and_then(|aux| aux.interactive_stop)
        .as_deref()
        == Some("edit")
    {
        reconcile_interactive_edit_continue(&mut state).await?;
        clear_interactive_stop()?;
    } else if RebaseAuxState::load_optional()?
        .and_then(|aux| aux.interactive_stop)
        .is_some()
    {
        clear_interactive_stop()?;
    }

    if let Some(stopped_sha) = state.stopped_sha {
        // Create a commit from the current index after the user has resolved
        // conflicts and staged the resolution.
        let index_file = path::index();
        let index = git_internal::internal::index::Index::load(&index_file)
            .map_err(|e| RebaseError::IndexLoad(e.to_string()))?;
        // Same guard: `--continue` builds a commit from the current index.
        crate::internal::layer::reject_layer_owned_entries(&index, "to continue the rebase")
            .await
            .map_err(RebaseError::IndexLoad)?;

        if has_unmerged_entries(&index) {
            return Err(RebaseError::UnresolvedConflicts);
        }

        // rerere: the conflict is resolved — record its postimage so an identical
        // conflict is auto-resolved next time. A no-op unless `rerere.enabled`.
        if let Err(error) =
            crate::command::rerere::auto_update(persisted_rerere_autoupdate()?).await
        {
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
                .await
                .map_err(|error| match error {
                    ReplayCommitError::Identity(detail) => RebaseError::IdentityMissing(detail),
                    ReplayCommitError::ObjectLoad(detail) => RebaseError::CommitLoad {
                        commit: state.current_head.to_string(),
                        detail,
                    },
                })?;
        save_object(&new_commit, &new_commit.id)
            .map_err(|e| RebaseError::CommitSave(e.to_string()))?;
        record_current_repo_commit_revision_with_predecessors_for_active_operation(
            new_commit.id.to_string(),
            replay_genealogy_predecessors(&original_commit, state.current_head, action),
        )
        .await
        .map_err(|error| RebaseError::CommitSave(error.to_string()))?;

        let previous_tip = state.current_head;
        state.current_head = new_commit.id;
        state.todo.pop_front();
        state.todo_actions.pop_front();
        state.done.push(stopped_sha);
        state.stopped_sha = None;

        let db = crate::internal::sequencer::request_db_checked()
            .await
            .map_err(RebaseError::StateSave)?;
        Head::update_result_with_conn(&db, Head::Detached(state.current_head), None)
            .await
            .map_err(|error| RebaseError::HeadUpdate(error.to_string()))?;

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
        if rebase_aux_is_interactive() {
            consume_applied_interactive_instruction()?;
        }
    }

    finish_replay_or_drive(
        &mut state,
        &branch,
        &onto_display,
        "continue",
        Some(previous_commit),
        applied_commits,
        dropped_commits,
        None,
        None,
        output,
    )
    .await
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
    crate::utils::index_ext::preserve_skip_worktree_from(&current_index, &mut index);
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
                Head::update_result_with_conn(txn, Head::Branch(branch_name_cloned), None)
                    .await
                    .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
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

async fn run_rebase_skip(output: &OutputConfig) -> Result<RebaseOutput, RebaseError> {
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
        return finish_replay_or_drive(
            &mut state,
            &branch,
            &onto_display,
            "skip",
            Some(previous_commit),
            Vec::new(),
            Vec::new(),
            None,
            None,
            output,
        )
        .await;
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
    if rebase_aux_is_interactive() {
        consume_applied_interactive_instruction()?;
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
    crate::utils::index_ext::preserve_skip_worktree_from(&current_index, &mut index);
    index
        .save(&index_file)
        .map_err(|e| RebaseError::IndexSave(e.to_string()))?;
    reset_workdir_tracked_only(&current_index, &index)
        .map_err(|e| RebaseError::WorkdirReset(e.to_string()))?;

    finish_replay_or_drive(
        &mut state,
        &branch,
        &onto_display,
        "skip",
        Some(previous_commit),
        Vec::new(),
        Vec::new(),
        Some(skipped_sha.to_string()),
        skipped_subject,
        output,
    )
    .await
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
    let mut items: HashMap<PathBuf, merge::MergeTreeEntry> = HashMap::new();
    for path in index.tracked_files() {
        let path_str = path_to_index_key(&path)?;
        if let Some(entry) = index.get(path_str, 0) {
            items.insert(
                path.clone(),
                merge::MergeTreeEntry {
                    hash: entry.hash,
                    mode: index_mode_to_tree_item_mode(entry.mode)?,
                },
            );
        }
    }
    merge::create_tree_from_items_map(&items)
}

#[cfg(not(unix))]
fn write_workdir_file(workdir: &Path, path: &Path, content: &[u8]) -> Result<(), String> {
    let file_path = workdir.join(path);
    crate::utils::worktree_blob::write_worktree_blob(&file_path, content, false)
        .map_err(|e| format!("failed to write {}: {}", file_path.display(), e))
}

fn write_rebase_workdir_entry(
    workdir: &Path,
    path: &Path,
    entry: merge::MergeTreeEntry,
) -> Result<(), String> {
    // Submodule pointers never reach the working tree (ADR-MG-01): the commit
    // object belongs to the submodule, so loading it as a blob would fail.
    if entry.mode == TreeItemMode::Commit {
        return Ok(());
    }
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
        TreeItemMode::Blob | TreeItemMode::BlobExecutable => {
            let file_path = workdir.join(path);
            crate::utils::worktree_blob::write_worktree_blob(
                &file_path,
                content,
                mode == TreeItemMode::BlobExecutable,
            )
            .map_err(|error| format!("failed to write {}: {error}", file_path.display()))
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;
    use std::str::FromStr;

    use clap::Parser;
    use git_internal::internal::object::tree::TreeItemMode;
    use tempfile::tempdir;

    #[cfg(unix)]
    use super::path_to_index_key;
    use super::{
        InteractiveReplayError, RebaseArgs, RebaseAuxState, RebaseError, RebaseState,
        RebaseTodoAction, ReplayErrorKind, decode_todo_actions_blob, encode_todo_actions_blob,
        index_mode_to_tree_item_mode, interactive_replay_items, rebase_start_spec,
        rerere_autoupdate_override, write_workdir_blob,
    };
    use crate::utils::error::{CliError, StableErrorCode};

    #[test]
    fn rebase_start_spec_root_remaps_positional_to_branch() {
        let args = RebaseArgs::try_parse_from(["rebase", "--root", "topic"])
            .expect("--root <branch> must parse");
        let spec = rebase_start_spec(&args).expect("single positional is <branch>");
        assert!(spec.root);
        assert_eq!(spec.upstream.as_deref(), None);
        assert_eq!(spec.branch.as_deref(), Some("topic"));
    }

    #[test]
    fn rebase_start_spec_root_rejects_upstream_and_branch() {
        let args = RebaseArgs::try_parse_from(["rebase", "--root", "main", "topic"])
            .expect("two positionals must parse");
        let err = rebase_start_spec(&args).expect_err("two positionals are usage");
        assert!(
            err.to_string()
                .contains("--root cannot be used together with <upstream>")
        );
    }

    #[test]
    fn rerere_autoupdate_flags_are_last_wins_and_old_aux_state_inherits() {
        let enabled = RebaseArgs::try_parse_from(["rebase", "--rerere-autoupdate", "main"])
            .expect("positive rerere toggle must parse");
        assert_eq!(rerere_autoupdate_override(&enabled), Some(true));

        let disabled = RebaseArgs::try_parse_from([
            "rebase",
            "--rerere-autoupdate",
            "--no-rerere-autoupdate",
            "main",
        ])
        .expect("the last rerere toggle must win");
        assert_eq!(rerere_autoupdate_override(&disabled), Some(false));

        let old: RebaseAuxState = serde_json::from_str("{}")
            .expect("sidecars written before rerere override remain readable");
        assert_eq!(old.rerere_autoupdate, None);
    }

    #[test]
    fn interactive_action_marker_roundtrip_and_old_reader_length() {
        use std::collections::VecDeque;

        use git_internal::hash::ObjectHash;

        let encoded = encode_todo_actions_blob("pick\npick".to_string(), true);
        assert_eq!(encoded, "interactive\npick\npick");
        let (interactive, tokens) = decode_todo_actions_blob(&encoded);
        assert!(interactive);
        assert_eq!(tokens, vec!["pick", "pick"]);

        let empty = encode_todo_actions_blob(String::new(), true);
        assert_eq!(empty, "interactive");
        let (interactive, tokens) = decode_todo_actions_blob(&empty);
        assert!(interactive);
        assert!(tokens.is_empty());

        let oid = ObjectHash::from_str("0123456789abcdef0123456789abcdef01234567").expect("oid");
        let todo = VecDeque::from([oid]);
        let actions = RebaseState::parse_action_list("interactive\npick", 1, false, &todo)
            .expect("marker + pick");
        assert_eq!(actions, VecDeque::from([RebaseTodoAction::Pick]));

        let err = RebaseState::parse_action_list("interactive\npick\npick", 1, false, &todo)
            .expect_err("unstripped marker must fail length check");
        assert!(err.contains("invalid todo_actions length"), "{err}");
    }

    #[test]
    fn interactive_replay_filters_pick_drop_and_rejects_other_ops() {
        use git_internal::hash::ObjectHash;

        use crate::command::rebase_todo::TodoInstruction;

        let a = ObjectHash::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("a");
        let b = ObjectHash::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").expect("b");
        let c = ObjectHash::from_str("cccccccccccccccccccccccccccccccccccccccc").expect("c");
        let known = [a, b, c];

        let picks = interactive_replay_items(
            &[
                TodoInstruction::Pick {
                    commit: "aaaaaaa".into(),
                },
                TodoInstruction::Drop {
                    commit: Some("bbbbbbb".into()),
                },
                TodoInstruction::Pick {
                    commit: "ccccccc".into(),
                },
            ],
            &known,
        )
        .expect("pick/drop");
        assert_eq!(
            picks,
            vec![(a, RebaseTodoAction::Pick), (c, RebaseTodoAction::Pick)]
        );

        let empty = interactive_replay_items(&[], &known).expect("empty");
        assert!(empty.is_empty());

        let err = interactive_replay_items(
            &[TodoInstruction::Squash {
                commit: "aaaaaaa".into(),
            }],
            &known,
        )
        .expect_err("leading squash is G5");
        assert_eq!(err, InteractiveReplayError::LeadingFold("squash"));

        let mapped = interactive_replay_items(
            &[
                TodoInstruction::Pick {
                    commit: "aaaaaaa".into(),
                },
                TodoInstruction::Fixup {
                    commit: "bbbbbbb".into(),
                    flag: Some(crate::command::rebase_todo::FixupFlag::KeepThis),
                },
                TodoInstruction::Fixup {
                    commit: "ccccccc".into(),
                    flag: Some(crate::command::rebase_todo::FixupFlag::Reword),
                },
            ],
            &known,
        )
        .expect("fixup -C/-c");
        assert_eq!(
            mapped,
            vec![
                (a, RebaseTodoAction::Pick),
                (b, RebaseTodoAction::FixupKeep),
                (c, RebaseTodoAction::FixupKeepEdit),
            ]
        );

        let stops = interactive_replay_items(
            &[
                TodoInstruction::Pick {
                    commit: "aaaaaaa".into(),
                },
                TodoInstruction::Edit {
                    commit: "bbbbbbb".into(),
                },
                TodoInstruction::Exec { cmd: "true".into() },
                TodoInstruction::Break,
            ],
            &known,
        )
        .expect("edit/break/exec");
        assert_eq!(
            stops,
            vec![(a, RebaseTodoAction::Pick), (b, RebaseTodoAction::Edit)]
        );
    }

    #[test]
    fn interactive_stop_messages_match_git_shape() {
        assert_eq!(
            super::format_stopped_at_edit("92d4069", "B"),
            "Stopped at 92d4069...  B\n\
You can amend the commit now with\n\
\n\
\tlibra commit --amend\n\
\n\
Once you are satisfied with your changes, run\n\
\n\
\tlibra rebase --continue"
        );
        assert_eq!(
            super::format_stopped_at_break("92d4069", "B"),
            "Stopped at 92d4069 (B)"
        );
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

        // A gitlink the replay would have to arbitrate is a DECLINED feature
        // (ADR-MG-01), not an IO or corruption failure.
        assert_eq!(
            ReplayErrorKind::GitlinkUnsupported.stable_code(),
            StableErrorCode::Unsupported
        );

        assert_eq!(
            ReplayErrorKind::MergeEngine.stable_code(),
            StableErrorCode::RepoStateInvalid
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
        assert_eq!(
            ReplayErrorKind::GitlinkUnsupported.as_str(),
            "gitlink_unsupported"
        );
        assert_eq!(ReplayErrorKind::MergeEngine.as_str(), "merge_engine");
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
        // ADR-MG-01: the refusal forwards the shared guard's wording verbatim
        // so `merge`, `rebase` and `cherry-pick` read identically.
        assert_eq!(
            RebaseError::GitlinkUnsupported(
                "rebase would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
                    .to_string(),
            )
            .to_string(),
            "rebase would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules",
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
    /// sweep (the retired terminal-control error / CherryPickError / RevertError /
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
        assert_eq!(
            code_of(RebaseError::InteractiveTodoHalted(
                "invalid command".to_string()
            )),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            code_of(RebaseError::InteractiveExecFailed {
                command: "false".to_string(),
                detail: String::new(),
            }),
            StableErrorCode::ConflictOperationBlocked,
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

    #[cfg(unix)]
    #[test]
    fn path_to_index_key_rejects_non_utf8_paths() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

        let path = PathBuf::from(OsString::from_vec(vec![0x66, 0x80]));
        let err = path_to_index_key(&path).expect_err("non-UTF-8 path should fail");
        assert!(err.contains("path is not valid UTF-8"));
    }

    #[test]
    fn rebase_index_tree_mode_conversions_pin_supported_modes() {
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
        assert_eq!(
            index_mode_to_tree_item_mode(0o160000).expect("gitlink"),
            TreeItemMode::Commit
        );
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
        assert_eq!(
            ReplayErrorKind::GitlinkUnsupported.to_string(),
            "gitlink_unsupported"
        );
        assert_eq!(ReplayErrorKind::MergeEngine.to_string(), "merge_engine");
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
async fn replay_commit_with_unified_merge(
    commit_to_replay_id: &ObjectHash,
    new_parent_id: &ObjectHash,
    action: RebaseTodoAction,
    empty_mode: RebaseEmptyMode,
    rerere_autoupdate: Option<bool>,
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

    // Unchanged pick: already parented on the new base, or `--root` without
    // `--onto` replaying the original root onto itself. Reuse the original
    // object so `--autosquash` on a linear history without fixup/squash and
    // `--root` on an unchanged history keep the original hashes (ADR-HF-12,
    // ADR-HF-13). Restore the index/worktree: start detaches HEAD to the
    // landing commit first, so a reuse-only replay would otherwise leave the
    // tree at the pre-rebase tip or the newbase.
    if action == RebaseTodoAction::Pick
        && (commit_to_replay.parent_commit_ids.first() == Some(new_parent_id)
            || commit_to_replay_id == new_parent_id)
    {
        if let Err(error) =
            restore_replay_index_and_workdir(&current_index, &index_file, &commit_to_replay.tree_id)
        {
            return error;
        }
        return ReplayResult::Success(*commit_to_replay_id);
    }
    let mut base_commits = Vec::with_capacity(commit_to_replay.parent_commit_ids.len());
    for parent_id in &commit_to_replay.parent_commit_ids {
        let base_commit: Commit = match load_object(parent_id) {
            Ok(commit) => commit,
            Err(error) => {
                return ReplayResult::internal(ReplayErrorKind::BaseTreeLoad, error.to_string());
            }
        };
        base_commits.push(base_commit);
    }
    // A flattened rebase still uses the first-parent comparison for its
    // historical `--empty=drop` decision. The tree merge itself receives all
    // parents and therefore uses a recursive virtual base for merge commits.
    // `--root --onto` replays a parentless commit against an empty base.
    let our_commit: Commit = match load_object(new_parent_id) {
        Ok(commit) => commit,
        Err(error) => {
            return ReplayResult::internal(ReplayErrorKind::OurTreeLoad, error.to_string());
        }
    };

    let new_tree_id =
        match merge::merge_rebase_trees(&base_commits, &our_commit, &commit_to_replay).await {
            Ok(merge::RebaseTreeMergeOutcome::Clean { tree_id }) => tree_id,
            Ok(merge::RebaseTreeMergeOutcome::Conflicted { paths }) => {
                if let Err(error) = crate::command::rerere::auto_update(rerere_autoupdate).await {
                    tracing::warn!("rerere auto-update after rebase conflict failed: {error}");
                }
                return ReplayResult::conflict(paths);
            }
            Err(error) => {
                return ReplayResult::internal(ReplayErrorKind::MergeEngine, error.to_string());
            }
        };

    // `--empty=drop`: a commit that BECOMES empty after replay (the merged tree
    // equals the new parent's tree — its changes are already on the new base) is
    // skipped. This is distinct from a commit that BEGINS empty (handled by
    // `--no-keep-empty` up front): the replayed commit's tree differs from its
    // original parent, confirming it introduced a change. The index/worktree
    // already equal the new parent when the result tree matches it, so no
    // mutation is needed before skipping. A parentless root is originally empty
    // only when its tree has no entries.
    let originally_empty = match base_commits.first() {
        Some(first_base_commit) => commit_to_replay.tree_id == first_base_commit.tree_id,
        None => load_object::<Tree>(&commit_to_replay.tree_id)
            .map(|tree| tree.tree_items.is_empty())
            .unwrap_or(false),
    };
    if empty_mode == RebaseEmptyMode::Drop && new_tree_id == our_commit.tree_id && !originally_empty
    {
        let subject = commit_subject_from_message(&commit_to_replay.message);
        return ReplayResult::BecameEmptyDropped { subject };
    }

    let new_commit = match create_replayed_commit(
        &commit_to_replay,
        new_tree_id,
        *new_parent_id,
        action,
    )
    .await
    {
        Ok(commit) => commit,
        Err(ReplayCommitError::Identity(detail)) => {
            return ReplayResult::internal(ReplayErrorKind::IdentityMissing, detail);
        }
        Err(error) => {
            return ReplayResult::internal(ReplayErrorKind::CommitLoad, error.into_detail());
        }
    };

    if let Err(e) = save_object(&new_commit, &new_commit.id) {
        return ReplayResult::internal(ReplayErrorKind::CommitSave, e.to_string());
    }
    if let Err(error) = record_current_repo_commit_revision_with_predecessors_for_active_operation(
        new_commit.id.to_string(),
        replay_genealogy_predecessors(&commit_to_replay, *new_parent_id, action),
    )
    .await
    {
        return ReplayResult::internal(ReplayErrorKind::CommitSave, error.to_string());
    }

    if let Err(error) = restore_replay_index_and_workdir(&current_index, &index_file, &new_tree_id)
    {
        return error;
    }

    ReplayResult::Success(new_commit.id)
}

fn restore_replay_index_and_workdir(
    current_index: &git_internal::internal::index::Index,
    index_file: &std::path::Path,
    tree_id: &ObjectHash,
) -> Result<(), ReplayResult> {
    let new_tree: Tree = match load_object(tree_id) {
        Ok(tree) => tree,
        Err(e) => {
            return Err(ReplayResult::internal(
                ReplayErrorKind::NewTreeLoad,
                e.to_string(),
            ));
        }
    };
    let mut index = git_internal::internal::index::Index::new();
    if let Err(e) = rebuild_index_from_tree(&new_tree, &mut index, "") {
        return Err(ReplayResult::internal(
            ReplayErrorKind::IndexRebuild,
            e.to_string(),
        ));
    }
    crate::utils::index_ext::preserve_skip_worktree_from(current_index, &mut index);
    if let Err(e) = index.save(index_file) {
        return Err(ReplayResult::internal(
            ReplayErrorKind::IndexSave,
            e.to_string(),
        ));
    }
    if let Err(e) = reset_workdir_tracked_only(current_index, &index) {
        return Err(ReplayResult::internal(
            ReplayErrorKind::WorkdirReset,
            e.to_string(),
        ));
    }
    Ok(())
}

/// Why building a replayed commit failed. A missing identity is a configuration
/// problem and an unreadable object is corruption; keeping them apart lets each
/// reach the user with the right stable code and the right fix.
enum ReplayCommitError {
    Identity(String),
    ObjectLoad(String),
}

impl ReplayCommitError {
    fn into_detail(self) -> String {
        match self {
            ReplayCommitError::Identity(detail) | ReplayCommitError::ObjectLoad(detail) => detail,
        }
    }
}

/// Build the commit that replaces `original_commit` on the new base.
///
/// Authorship follows Git's rebase semantics, which `Commit::from_tree_id`
/// cannot express because it hardcodes `mega <admin@mega.org>` for both
/// signatures:
///
/// * **author** is *preserved*, never re-stamped — a rebase rewrites history's
///   shape, not its authorship. `pick` keeps the replayed commit's own author;
///   `fixup`/`squash`/`amend` fold into an earlier commit, so the result keeps
///   *that* commit's author (Git: the author of the first commit in the group),
///   which is why they read it off `target` rather than `original_commit`.
/// * **committer** is the person running the rebase, resolved exactly as
///   `libra commit` resolves it, with a fresh timestamp.
async fn create_replayed_commit(
    original_commit: &Commit,
    tree_id: ObjectHash,
    new_parent_id: ObjectHash,
    action: RebaseTodoAction,
) -> Result<Commit, ReplayCommitError> {
    let (committer, _) = crate::command::commit::create_committer_signature()
        .await
        .map_err(|error| ReplayCommitError::Identity(error.to_string()))?;
    match action {
        RebaseTodoAction::Pick => Ok(Commit::new(
            original_commit.author.clone(),
            committer,
            tree_id,
            vec![new_parent_id],
            &original_commit.message,
        )),
        RebaseTodoAction::Fixup => {
            let target: Commit = load_object(&new_parent_id)
                .map_err(|error| ReplayCommitError::ObjectLoad(error.to_string()))?;
            Ok(Commit::new(
                target.author.clone(),
                committer,
                tree_id,
                target.parent_commit_ids.clone(),
                &target.message,
            ))
        }
        RebaseTodoAction::Squash => {
            let target: Commit = load_object(&new_parent_id)
                .map_err(|error| ReplayCommitError::ObjectLoad(error.to_string()))?;
            let (target_clean, _) = parse_commit_msg(&target.message);
            let (this_clean, _) = parse_commit_msg(&original_commit.message);
            let message = format!("{}\n\n{}", target_clean.trim(), this_clean.trim());
            Ok(Commit::new(
                target.author.clone(),
                committer,
                tree_id,
                target.parent_commit_ids.clone(),
                &message,
            ))
        }
        RebaseTodoAction::Amend => {
            let target: Commit = load_object(&new_parent_id)
                .map_err(|error| ReplayCommitError::ObjectLoad(error.to_string()))?;
            let message = amend_replacement_message(&original_commit.message);
            Ok(Commit::new(
                target.author.clone(),
                committer,
                tree_id,
                target.parent_commit_ids.clone(),
                &message,
            ))
        }
        RebaseTodoAction::Reword | RebaseTodoAction::Edit => Ok(Commit::new(
            original_commit.author.clone(),
            committer,
            tree_id,
            vec![new_parent_id],
            &original_commit.message,
        )),
        RebaseTodoAction::FixupKeep | RebaseTodoAction::FixupKeepEdit => {
            let target: Commit = load_object(&new_parent_id)
                .map_err(|error| ReplayCommitError::ObjectLoad(error.to_string()))?;
            let (this_clean, _) = parse_commit_msg(&original_commit.message);
            Ok(Commit::new(
                target.author.clone(),
                committer,
                tree_id,
                target.parent_commit_ids.clone(),
                this_clean.trim(),
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
/// First-parent walk from `head_id` inclusive through the root commit,
/// oldest first. Used by `rebase --root` (ADR-HF-13).
async fn collect_commits_from_root(head_id: &ObjectHash) -> Result<Vec<ObjectHash>, String> {
    let mut commits = Vec::new();
    let mut current_id = *head_id;
    loop {
        commits.push(current_id);
        let commit: Commit = load_object(&current_id).map_err(|e| e.to_string())?;
        if commit.parent_commit_ids.is_empty() {
            break;
        }
        current_id = commit.parent_commit_ids[0];
    }
    commits.reverse();
    Ok(commits)
}

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

/// `ORIG_HEAD` for an explicitly resolved scope — the pseudo-ref projection
/// (§C.5). `None` when that worktree has no rebase in progress; an unreadable
/// row is an ERROR, never "nothing in progress".
pub(crate) async fn orig_head_for_scope(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> Result<Option<String>, String> {
    Ok(RebaseState::load_for_scope(scope)
        .await?
        .map(|state| state.orig_head.to_string()))
}

/// `REBASE_HEAD` for an explicitly resolved scope: the commit a STOPPED rebase
/// is sitting on. A rebase in progress that has not stopped defines no
/// `REBASE_HEAD`, which is why the column is nullable.
pub(crate) async fn stopped_sha_for_scope(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> Result<Option<String>, String> {
    Ok(RebaseState::load_for_scope(scope)
        .await?
        .and_then(|state| state.stopped_sha.map(|sha| sha.to_string())))
}

/// Reset the working directory to match the new index state without overwriting untracked files.
fn reset_workdir_tracked_only(
    current_index: &git_internal::internal::index::Index,
    new_index: &git_internal::internal::index::Index,
) -> Result<(), String> {
    let workdir = util::request_working_dir();
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
            // A submodule directory is not Libra's to unlink, and a gitlink can
            // only leave the index through a decision the ADR-MG-01 guard has
            // already refused.
            if current_index
                .get(path_to_index_key(&path_buf)?, 0)
                .is_some_and(|entry| entry.mode & 0o170000 == 0o160000)
            {
                continue;
            }
            let full_path = workdir.join(path_buf);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|e| e.to_string())?;
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        let path_str = path_to_index_key(&path_buf)?;
        if let Some(entry) = new_index.get(path_str, 0) {
            // Pass-through gitlink: nothing to materialize in the working tree.
            if entry.mode & 0o170000 == 0o160000 {
                continue;
            }
            let mode = index_mode_to_tree_item_mode(entry.mode)?;
            write_rebase_workdir_entry(
                &workdir,
                &path_buf,
                merge::MergeTreeEntry {
                    hash: entry.hash,
                    mode,
                },
            )?;
        }
    }

    Ok(())
}

fn path_to_index_key(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn index_mode_to_tree_item_mode(mode: u32) -> Result<TreeItemMode, String> {
    match mode {
        0o100644 => Ok(TreeItemMode::Blob),
        0o100755 => Ok(TreeItemMode::BlobExecutable),
        0o120000 => Ok(TreeItemMode::Link),
        0o160000 => Ok(TreeItemMode::Commit),
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
            // A `160000` gitlink records a SUBMODULE's commit id, which is not
            // an object of this repository, so it has no blob to size. Only a
            // pass-through gitlink can reach here — an arbitrated one is refused
            // by the ADR-MG-01 guard before the replay writes anything — so the
            // pointer is recorded verbatim instead of failing the rebase.
            git_internal::internal::object::tree::TreeItemMode::Commit => {
                let mut entry =
                    git_internal::internal::index::IndexEntry::new_from_blob(full_path, item.id, 0);
                entry.mode = 0o160000;
                index.add(entry);
                continue;
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
    #[serial_test::serial(cwd, env)]
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
    #[serial_test::serial(cwd, env)]
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
    #[serial_test::serial(cwd, env)]
    async fn rebuild_index_from_tree_registers_gitlink_entries_verbatim() {
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

        // ADR-MG-01 pass-through: an arbitrated gitlink is refused by
        // `merge::ensure_gitlinks_not_arbitrated` before the replay writes
        // anything, so the index rebuild only ever sees pointers all three
        // sides agree on — and must record them rather than fail. The commit
        // object is a SUBMODULE's, absent from this object database, so asking
        // for its blob was the wrong question.
        rebuild_index_from_tree(&tree, &mut index, "").expect("gitlink entry is registered");

        let entry = index.get("vendor", 0).expect("gitlink index entry");
        assert_eq!(entry.mode, 0o160000);
        assert_eq!(entry.hash, gitlink);
    }
}
