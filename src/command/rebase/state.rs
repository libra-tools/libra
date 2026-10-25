//! Rebase state persistence: the merge/autostash sidecar, scope-aware ref
//! updates and GC roots shared by the interactive and replay drivers.
#![allow(unused_imports)]
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
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

use super::*;
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
    pub(super) exec_commands: Vec<String>,
    /// Index of the command that must be retried by `rebase --continue` after
    /// an `--exec` failure. `None` means no command is pending.
    #[serde(default)]
    pub(super) pending_exec: Option<usize>,
    #[serde(default)]
    pub(super) update_refs: bool,
    /// `rebase --signoff` trailer (`Signed-off-by: Name <email>`), resolved at
    /// rebase start and appended to every replayed commit. Durable so
    /// `--continue` keeps signing the remaining commits (issues/523).
    #[serde(default)]
    pub(super) signoff: Option<String>,
    /// Branches selected at rebase start. Checked-out branches are excluded.
    #[serde(default)]
    pub(super) refs_to_update: Vec<RebaseRefUpdate>,
    /// Original commit -> rewritten commit, populated after every replayed or
    /// dropped commit so update-refs survives conflicts and process restarts.
    #[serde(default)]
    pub(super) rewrites: BTreeMap<String, String>,
    /// Original start-empty commit -> its original parent (or the new base).
    /// Used to resolve branches pointing at commits removed by
    /// `--no-keep-empty` once their nearest retained ancestor is rewritten.
    #[serde(default)]
    pub(super) rewrite_aliases: BTreeMap<String, String>,
    /// Held stash commit, deliberately outside `refs/stash` until the rebase
    /// completes or aborts.
    #[serde(default)]
    pub(super) autostash: Option<String>,
    /// Explicit rerere staging choice for this rebase. Missing fields from
    /// older sidecars inherit the current `rerere.autoUpdate` configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rerere_autoupdate: Option<bool>,
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
    pub(super) interactive_known: Vec<String>,
    /// Why an interactive rebase is paused: `edit`, `break`, or `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) interactive_stop: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RebaseRefUpdate {
    pub(super) branch: String,
    pub(super) old_oid: String,
}

impl RebaseAuxState {
    /// Part C W1 (§C.4.2): the aux sidecar (exec queue, update-refs plan,
    /// rewrites, held autostash oid) is per-rebase state, so it lives in THIS
    /// worktree's local gitdir. For the main worktree the local gitdir IS the
    /// common `.libra`, so main-worktree paths are unchanged.
    pub(super) fn path() -> PathBuf {
        util::request_worktree_gitdir_strict().join("rebase-aux.json")
    }

    pub(super) fn load_optional() -> Result<Option<Self>, RebaseError> {
        Self::load_optional_at(&Self::path())
    }

    pub(super) fn load_optional_at(path: &std::path::Path) -> Result<Option<Self>, RebaseError> {
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

    pub(super) fn save(&self) -> Result<(), RebaseError> {
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

    pub(super) fn marks_interactive(&self) -> bool {
        !self.todo_instructions.is_empty()
            || !self.done_instructions.is_empty()
            || self.interactive_parse_error.is_some()
            || self.interactive_todo_text.is_some()
    }

    pub(super) fn cleanup() -> Result<(), RebaseError> {
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
            crate::internal::object_format::parse_repo_oid(&oid).map_err(|error| {
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
    pub(super) fn legacy_rebase_dir_present() -> Option<PathBuf> {
        let storage = util::request_storage_path();
        Self::LEGACY_REBASE_DIRS
            .iter()
            .map(|name| storage.join(name))
            .find(|path| path.exists())
    }

    pub(super) fn legacy_rebase_dir() -> PathBuf {
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
    pub(super) fn legacy_state_is_adoptable() -> Result<bool, String> {
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

    pub(super) fn ambiguous_legacy_message(legacy_dir: &Path) -> String {
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
    /// performed by a control action through `Self::adopt_legacy_state`.
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
    pub(super) fn scope_key() -> String {
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

    pub(super) async fn has_state_in_db<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
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

    pub(super) async fn load_from_db<C: ConnectionTrait>(db: &C) -> Result<Option<Self>, String> {
        Self::load_from_db_in_scope(db, &Self::scope_key()).await
    }

    pub(super) async fn load_from_db_in_scope<C: ConnectionTrait>(
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

        let onto = crate::internal::object_format::parse_repo_oid(onto_str.trim())
            .map_err(|e| format!("Invalid onto hash: {e}"))?;
        let orig_head = crate::internal::object_format::parse_repo_oid(orig_head_str.trim())
            .map_err(|e| format!("Invalid orig_head hash: {e}"))?;
        let current_head = crate::internal::object_format::parse_repo_oid(current_head_str.trim())
            .map_err(|e| format!("Invalid current_head hash: {e}"))?;
        let todo = VecDeque::from(Self::parse_hash_list(&todo_str)?);
        let autosquash = autosquash_value != 0;
        let todo_actions =
            Self::parse_action_list(&todo_actions_str, todo.len(), autosquash, &todo)?;
        let done = Self::parse_hash_list(&done_str)?;
        let stopped_sha = match stopped_str {
            Some(s) if !s.trim().is_empty() => Some(
                crate::internal::object_format::parse_repo_oid(s.trim())
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

    pub(super) async fn save_with_conn<C: ConnectionTrait>(
        db: &C,
        state: &RebaseState,
    ) -> Result<(), String> {
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
    pub(super) async fn insert_with_conn<C: ConnectionTrait>(
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

    pub(super) async fn clear_state_in_db<C: ConnectionTrait>(db: &C) -> Result<(), String> {
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

    pub(super) async fn migrate_legacy_state<C: ConnectionTrait>(
        db: &C,
    ) -> Result<Option<Self>, String> {
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

    pub(super) fn load_from_legacy_dir() -> Result<Self, String> {
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
        let onto = crate::internal::object_format::parse_repo_oid(onto_str.trim())
            .map_err(|e| format!("Invalid onto hash: {}", e))?;

        let orig_head_str = fs::read_to_string(dir.join("orig-head"))
            .map_err(|e| format!("Failed to read orig-head: {}", e))?;
        let orig_head = crate::internal::object_format::parse_repo_oid(orig_head_str.trim())
            .map_err(|e| format!("Invalid orig-head hash: {}", e))?;

        let current_head_str = fs::read_to_string(dir.join("current-head"))
            .map_err(|e| format!("Failed to read current-head: {}", e))?;
        let current_head = crate::internal::object_format::parse_repo_oid(current_head_str.trim())
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
                crate::internal::object_format::parse_repo_oid(stopped_str.trim())
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

    pub(super) fn parse_hash_list(content: &str) -> Result<Vec<ObjectHash>, String> {
        let mut commits = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let hash = crate::internal::object_format::parse_repo_oid(trimmed)
                    .map_err(|e| format!("Invalid commit hash '{}': {}", trimmed, e))?;
                commits.push(hash);
            }
        }
        Ok(commits)
    }

    pub(super) fn parse_action_list(
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

    pub(super) fn default_todo_actions(
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

    pub(super) fn format_hash_list(list: impl IntoIterator<Item = ObjectHash>) -> String {
        let mut out = String::new();
        for (idx, hash) in list.into_iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&hash.to_string());
        }
        out
    }

    pub(super) fn format_action_list(list: impl IntoIterator<Item = RebaseTodoAction>) -> String {
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
