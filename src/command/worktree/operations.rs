//! Worktree normal operations: add/reattach/list/lock/unlock/move/prune/remove/umount
//! plus the lifecycle/journal persistence helpers they share.
#![allow(unused_imports)]
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
pub(crate) use lock::acquire_registry_lock_async;
pub(crate) use registry::{
    DETACHED_MARKER, LinkedHistory, WorktreeEntry, WorktreeEntryState, WorktreeState,
    local_gitdir_for_scope, registry_knows_linked_worktree,
    registry_knows_linked_worktree_in_storage,
};
#[cfg(test)]
use registry::{REGISTRY_SCHEMA_VERSION, WorktreeStateV1};
use registry::{
    RegistryShape, canonicalize, ensure_main_entry, find_entry, find_entry_mut, load_state,
    load_state_for_repair, load_state_readonly, load_state_readonly_at, normalize_v2_ids,
    resolve_worktree_id, save_state, state_path, write_state,
};
use serde::Serialize;

use self::doctor::*;
use super::*;
#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    command::restore::{self, RestoreArgs},
    internal::{branch::Branch, head::Head, sequencer::WorktreeControl, workspace::RepoIdentity},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// Manage multiple working trees attached to this repository.
//
// Note: the user-facing summary for `libra worktree --help` is set via
// `#[command(about = "...", long_about = ...)]` on the Cli enum binding
// in src/cli.rs. We use `long_about` here so clap renders the same one-
// liner in both the top-level command list and `worktree --help`'s
// header, instead of leaking the previous "CLI arguments for the
// `worktree` subcommand. This type is wired into..." rustdoc body.
pub(crate) async fn add_worktree(
    path: String,
    target_spec: Option<String>,
    detach: bool,
    new_branch: Option<String>,
) -> WorktreeResult<WorktreeAddOutput> {
    // Registry mutation lock: the whole precheck → sweep → seed → registry
    // write sequence runs under it (a concurrent add's sweep must not
    // delete this add's freshly seeded rows).
    let _registry_lock = acquire_registry_lock_async().await?;
    let _repository_ref_lease = acquire_worktree_ref_lease().await?;
    let storage = util::storage_path();
    let target = resolve_path(&path, "worktree path")?;

    if util::is_sub_path(&target, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "worktree path cannot be inside .libra storage: {}",
            target.display()
        )));
    }

    let target_exists = target.exists();
    if target_exists && !target.is_dir() {
        return Err(WorktreeError::InvalidTarget(format!(
            "target exists and is not a directory: {}",
            target.display()
        )));
    }

    let canonical_target = resolve_path(&target, "worktree path")?;
    if util::is_sub_path(&canonical_target, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "worktree path cannot be inside .libra storage: {}",
            canonical_target.display()
        )));
    }

    let mut state = load_state()?;
    if let Some(existing_index) = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == canonical_target)
    {
        match state.entries[existing_index].state {
            // W3-s1b (§C.7): re-adding a DETACHED worktree re-attaches it —
            // the frozen directory resumes with ITS OWN scoped state, so a
            // checkout target would be silently ignored: refuse it.
            WorktreeEntryState::DetachedFromRegistry => {
                if target_spec.is_some() || detach || new_branch.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is a detached worktree; re-attaching resumes its own \
                          HEAD — drop the branch/commit arguments (then switch inside it)",
                        canonical_target.display()
                    )));
                }
                return reattach_worktree(&mut state, existing_index, &canonical_target).await;
            }
            WorktreeEntryState::Tombstone => {
                return Err(WorktreeError::OperationBlocked(format!(
                    "'{}' is a tombstone (scoped cleanup pending); run `libra worktree \
                      repair --confirm` first, then add",
                    canonical_target.display()
                )));
            }
            WorktreeEntryState::Active => {
                if target_spec.is_some() || detach || new_branch.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is already a registered worktree; switch branches inside \
                          it instead",
                        canonical_target.display()
                    )));
                }
                return Ok(WorktreeAddOutput {
                    path: canonical_target.to_string_lossy().to_string(),
                    already_exists: true,
                    reattached: false,
                });
            }
        }
    }

    if target_exists
        && fs::read_dir(&target)
            .map_err(|source| {
                WorktreeError::IoRead(format!(
                    "failed to read target directory '{}': {source}",
                    target.display()
                ))
            })?
            .next()
            .transpose()
            .map_err(|source| {
                WorktreeError::IoRead(format!(
                    "failed to read target directory '{}': {source}",
                    target.display()
                ))
            })?
            .is_some()
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "target directory exists and is not empty: {}",
            target.display()
        )));
    }

    // W3-s2 (§C.7): resolve the checkout target FAIL-CLOSED before any side
    // effect — a bad branch/commit, a branch checked out in any scope, or a
    // `-b` collision must refuse with the directory, branch set, registry,
    // and journal untouched.
    let source_commit = Head::current_commit_result().await.map_err(|e| {
        WorktreeError::IoRead(format!(
            "failed to read HEAD while resolving the target: {e}"
        ))
    })?;
    let checkout = if let Some(name) = new_branch {
        if detach {
            return Err(WorktreeError::InvalidTarget(
                "-b/--create-branch cannot be combined with --detach".to_string(),
            ));
        }
        if Branch::find_branch_result(&name, None)
            .await
            .map_err(|e| WorktreeError::IoRead(format!("failed to look up branch: {e}")))?
            .is_some()
        {
            return Err(WorktreeError::OperationBlocked(format!(
                "branch '{name}' already exists; -B/--force are not supported — pick a new \
                  name or check the existing branch out with `worktree add <path> {name}`"
            )));
        }
        let start = match &target_spec {
            Some(spec) => util::get_commit_base(spec).await.map_err(|error| {
                WorktreeError::InvalidTarget(format!(
                    "cannot resolve start-point '{spec}': {error}"
                ))
            })?,
            None => source_commit.ok_or_else(|| {
                WorktreeError::OperationBlocked(
                    "cannot create a branch in an unborn repository (no commits yet)".to_string(),
                )
            })?,
        };
        AddCheckout::CreateBranch { name, start }
    } else if let Some(spec) = &target_spec {
        match Branch::find_branch_result(spec, None)
            .await
            .map_err(|e| WorktreeError::IoRead(format!("failed to look up branch: {e}")))?
        {
            Some(branch) => {
                if detach {
                    AddCheckout::Detached(branch.commit)
                } else {
                    // Branches are SHARED: refuse when ANY scope (including
                    // the one running this command) has the branch out. The
                    // probe is Result-returning — a query failure refuses
                    // (fail closed), never reads as "the branch is free".
                    match Head::branch_checked_out_anywhere_result(spec).await {
                        Err(error) => {
                            return Err(WorktreeError::IoRead(format!(
                                "cannot verify whether branch '{spec}' is checked out: \
                                  {error}"
                            )));
                        }
                        Ok(Some(scope)) => {
                            return Err(WorktreeError::OperationBlocked(format!(
                                "branch '{spec}' is already checked out at worktree \
                                  '{scope}'; use --detach to share its tip read-only"
                            )));
                        }
                        Ok(None) => {}
                    }
                    AddCheckout::AttachBranch { name: spec.clone() }
                }
            }
            None => {
                let commit = util::get_commit_base(spec).await.map_err(|error| {
                    WorktreeError::InvalidTarget(format!(
                        "'{spec}' is neither a local branch nor a resolvable commit \
                          ({error}); Libra does not create branches from remote-tracking \
                          names automatically (Git's DWIM is deferred) — use `-b {spec} \
                          <path> <remote>/{spec}` explicitly"
                    ))
                })?;
                AddCheckout::Detached(commit)
            }
        }
    } else {
        AddCheckout::DetachedAtSource
    };

    // §C.7 identity preflight, BEFORE the directory exists: instance ids are
    // path-derived, so a collision means another entry already claims this
    // identity. Persisting it would produce the registry state `validate_v2`
    // refuses to load — locking the user out of every worktree command — and
    // creating the directory first would leave an unregistered one behind on
    // the refusal path.
    {
        let candidate = util::worktree_instance_id(
            &std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone()),
        );
        if let Some(conflict) = state.identity_conflict_with(&candidate) {
            return Err(WorktreeError::OperationBlocked(format!(
                "cannot register worktree '{}': {conflict}",
                target.display()
            )));
        }
    }

    let mut created_target = false;
    if !target.exists() {
        fs::create_dir_all(&target).map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create worktree directory '{}': {source}",
                target.display()
            ))
        })?;
        created_target = true;
    }

    let link_path = target.join(util::ROOT_DIR);
    if link_path.exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "target already contains a .libra entry: {}",
            link_path.display()
        )));
    }

    let worktree_id = util::worktree_instance_id(&canonical_target);
    // W1 §C.4.1.1: instance ids are DETERMINISTIC (path-derived), so a
    // worktree re-added where one was previously removed would inherit any
    // scoped rows a best-effort remove/prune GC failed to delete — stale
    // sparse filters would silently re-gate ls-files/diff/hydrate, stale
    // layer ownership would block staging. Sweep the scope STRICTLY before
    // seeding; a sweep failure fails the add (fail closed, nothing seeded).
    let db = crate::internal::db::get_db_conn_instance().await;
    gc_worktree_scoped_rows_strict(&db, &worktree_id, true)
        .await
        .map_err(|e| {
            WorktreeError::IoWrite(format!(
                "cannot register worktree '{}': failed to clear stale scoped rows for its \
                  instance id: {e}",
                target.display()
            ))
        })?;
    // Durable intent for the whole gitdir/populate/registry window (§C.7).
    // Failure paths below roll the filesystem back themselves; a CRASH
    // leaves this row for `worktree repair`, whose `add` recovery sweeps
    // the scope and resolves it (directories are never deleted in
    // recovery).
    let mut add_payload = serde_json::json!({ "path": canonical_target.to_string_lossy() });
    if let AddCheckout::CreateBranch { name, start } = &checkout {
        // Recovery must be able to roll the `-b` branch back tip-
        // conditionally if we crash between its creation and publication.
        add_payload["create_branch"] = serde_json::json!({
            "name": name,
            "start": start.to_string(),
        });
    }
    let add_journal_id = journal_append(
        &db,
        WorktreeControl::Add.declare(),
        Some(&worktree_id),
        &add_payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    create_worktree_gitdir(&storage, &link_path, &worktree_id).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "failed to create per-worktree .libra gitdir in '{}': {source}",
            link_path.display()
        ))
    })?;

    let rollback_partial_add = || {
        let _ = remove_worktree_storage_link(&link_path);
        if created_target {
            let _ = fs::remove_dir_all(&target);
        } else if let Ok(entries) = fs::read_dir(&target) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                let _ = if entry_path.is_dir() {
                    fs::remove_dir_all(&entry_path)
                } else {
                    fs::remove_file(&entry_path)
                };
            }
        }
    };

    // W3-s2: the seed HEAD per resolved checkout mode. `source_commit` was
    // read via the RESULT-returning API before any side effect — only a
    // genuinely unborn HEAD (None) skips seeding, and only for the
    // no-target mode (explicit targets always carry a commit).
    // Branch-attach lock (W3-s2 §C.7): held from the final
    // checked-out-anywhere re-check through the HEAD seed, serializing with
    // `switch`/`checkout` (which hold it across their check + publication).
    let _attach_lock = if matches!(
        &checkout,
        AddCheckout::AttachBranch { .. } | AddCheckout::CreateBranch { .. }
    ) {
        match util::acquire_branch_attach_lock() {
            Ok(guard) => Some(guard),
            Err(error) => {
                rollback_partial_add();
                // A HANDLED failure fully rolled back — resolve the intent row
                // too, or repair keeps sweeping a scope that no longer exists.
                // Best-effort: an unresolved row is the crash contract anyway.
                let _ = journal_resolve(&db, add_journal_id).await;
                return Err(WorktreeError::IoWrite(format!(
                    "cannot acquire the branch-attach lock: {error}"
                )));
            }
        }
    } else {
        None
    };
    let (seed_head, seed_commit, created_branch): (Option<Head>, Option<_>, Option<(String, _)>) =
        match &checkout {
            AddCheckout::DetachedAtSource => {
                (source_commit.map(Head::Detached), source_commit, None)
            }
            AddCheckout::Detached(commit) => (Some(Head::Detached(*commit)), Some(*commit), None),
            AddCheckout::AttachBranch { name } => {
                // Final re-check UNDER the branch-attach lock, just before
                // the attach becomes durable — over EVERY scope (a
                // concurrent switch may have moved THIS worktree onto the
                // branch), and fail-closed on query errors.
                match Head::branch_checked_out_anywhere_result(name).await {
                    Err(error) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "cannot verify whether branch '{name}' is checked out: {error}"
                        )));
                    }
                    Ok(Some(scope)) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' is already checked out at worktree \
                              '{scope}'; use --detach to share its tip read-only"
                        )));
                    }
                    Ok(None) => {}
                }
                let branch = match Branch::find_branch_result(name, None).await {
                    Ok(Some(branch)) => branch,
                    Ok(None) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::InvalidTarget(format!(
                            "branch '{name}' disappeared while creating the worktree"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "failed to re-read branch '{name}': {e}"
                        )));
                    }
                };
                (Some(Head::Branch(name.clone())), Some(branch.commit), None)
            }
            AddCheckout::CreateBranch { name, start } => {
                // Collision re-check UNDER the branch-attach lock: the
                // preflight ran before the lock, and `update_branch` would
                // silently overwrite an existing row — a concurrent
                // `add -b <same-name>` must lose here, not double-attach.
                match Branch::find_branch_result(name, None).await {
                    Ok(None) => {}
                    Ok(Some(_)) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' was created concurrently; pick another name"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "failed to re-check branch '{name}': {e}"
                        )));
                    }
                }
                // Create the branch row NOW (cwd is still the source
                // worktree); every failure below deletes it back
                // tip-conditionally — no branch-only residue.
                if let Err(e) = Branch::update_branch(name, &start.to_string(), None).await {
                    rollback_partial_add();
                    // A HANDLED failure fully rolled back — resolve the intent row
                    // too, or repair keeps sweeping a scope that no longer exists.
                    // Best-effort: an unresolved row is the crash contract anyway.
                    let _ = journal_resolve(&db, add_journal_id).await;
                    return Err(WorktreeError::IoWrite(format!(
                        "failed to create branch '{name}': {e}"
                    )));
                }
                (
                    Some(Head::Branch(name.clone())),
                    Some(*start),
                    Some((name.clone(), *start)),
                )
            }
        };
    let rollback_created_branch = |created: Option<(String, git_internal::hash::ObjectHash)>| async move {
        if let Some((name, tip)) = created {
            match Branch::delete_branch_if_tip_result(&name, &tip).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        branch = name,
                        %error,
                        "could not roll back the created branch; delete it manually"
                    );
                }
            }
        }
    };
    if let (Some(seed_head), Some(commit)) = (seed_head, seed_commit) {
        let _ = commit;
        let _guard = match DirGuard::change_to(&target) {
            Ok(g) => g,
            Err(e) => {
                rollback_partial_add();
                rollback_created_branch(created_branch).await;
                // A HANDLED failure fully rolled back — resolve the intent row
                // too, or repair keeps sweeping a scope that no longer exists.
                // Best-effort: an unresolved row is the crash contract anyway.
                let _ = journal_resolve(&db, add_journal_id).await;
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter worktree directory '{}': {e}",
                    target.display()
                )));
            }
        };
        let created_branch = created_branch.clone();
        // §C.4.2: this block acts on ANOTHER worktree than the one the user
        // invoked from, so it re-pins rather than inheriting the invoker's
        // pin. Without this the pinned resolvers — `path::index()` above all
        // — would seed the INVOKER's index from the new worktree's HEAD:
        // main's staged state overwritten, and the new worktree left with an
        // empty index in which every checked-out file reads as deleted.
        let _scope = crate::internal::worktree_scope::WorktreeScope::override_scope(target.clone());
        // lore.md 2.1: seed the NEW worktree's OWN HEAD per the resolved
        // checkout (detached commit, attached branch, or the just-created
        // `-b` branch), so `Head::current()` resolves here and the populate
        // below can read it. A seed-update failure rolls EVERYTHING back —
        // including a `-b` branch row.
        if let Err(e) = Head::update_result(seed_head, None).await {
            // Both the scope and the cwd go back to the invoker BEFORE the
            // rollbacks: those act on the invoking repository's rows.
            drop(_scope);
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
            // A HANDLED failure fully rolled back — resolve the intent row
            // too, or repair keeps sweeping a scope that no longer exists.
            // Best-effort: an unresolved row is the crash contract anyway.
            let _ = journal_resolve(&db, add_journal_id).await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to seed HEAD for worktree '{}': {e}",
                target.display()
            )));
        }
        // Populate from HEAD so new worktrees reflect committed state instead
        // of carrying staged-but-uncommitted index content.
        if let Err(e) = restore::execute_checked(RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            pathspec: vec![util::working_dir_string()],
            source: Some("HEAD".to_string()),
            worktree: true,
            // lore.md 2.1: also restore the PRIVATE index to HEAD (a linked
            // worktree no longer shares the main index, so a fresh worktree's
            // index must be seeded to match HEAD or every file reads as a
            // phantom change).
            staged: true,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        })
        .await
        {
            // Restore the invoker's scope and cwd BEFORE the rollbacks:
            // deleting the target while it is the cwd would break the branch
            // rollback's storage resolution (and strand the shell in a
            // removed dir).
            drop(_scope);
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
            // A HANDLED failure fully rolled back — resolve the intent row
            // too, or repair keeps sweeping a scope that no longer exists.
            // Best-effort: an unresolved row is the crash contract anyway.
            let _ = journal_resolve(&db, add_journal_id).await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to populate worktree '{}': {e}",
                target.display()
            )));
        }
    }

    let registration_epoch = state.next_epoch();
    // A linked worktree exists from here on, and that fact must OUTLIVE its
    // entry: the ambiguous-sidecar rules ask "did one ever exist", and a later
    // removal deletes the entry (§C.4.3).
    state.linked_history = LinkedHistory::Existed;
    state.entries.push(WorktreeEntry {
        path: canonical_target.to_string_lossy().to_string(),
        is_main: false,
        locked: false,
        lock_reason: None,
        // v2 (§C.7): persist the stable id at creation so `worktree repair
        // <path>` can later restore a corrupt/missing gitdir identity from
        // the registry.
        worktree_id: Some(worktree_id.clone()),
        state: WorktreeEntryState::Active,
        // A fresh generation for every registration, so a client fenced on the
        // previous one at this same path/id is refused rather than served.
        epoch: registration_epoch,
    });
    if let Err(e) = write_state(&state) {
        rollback_partial_add();
        // A HANDLED failure fully rolled back — resolve the intent row
        // too, or repair keeps sweeping a scope that no longer exists.
        // Best-effort: an unresolved row is the crash contract anyway.
        let _ = journal_resolve(&db, add_journal_id).await;
        if let AddCheckout::CreateBranch { name, start } = &checkout
            && let Err(error) = Branch::delete_branch_if_tip_result(name, start).await
        {
            tracing::warn!(
                branch = name,
                %error,
                "could not roll back the created branch; delete it manually"
            );
        }
        return Err(e);
    }
    if let Err(error) = journal_resolve(&db, add_journal_id).await {
        tracing::warn!(
            error,
            "add journal entry not resolved; repair will reconcile"
        );
    }

    // W0 §C.4.1.1 (plan line 2258, 2026-08-06 revision): probe the TARGET
    // filesystem's case behavior and WARN when it disagrees with the
    // repository's persisted `core.ignorecase` — persisted at init from
    // MAIN's filesystem, which this worktree may not share. The per-worktree
    // config overlay that will store this probe rides the W4 unified
    // resolver; until then the mismatch must at least be visible, not
    // silent (materialization guards fall back to a live per-use probe only
    // when the config key is UNSET).
    warn_on_case_probe_mismatch(&canonical_target).await;

    Ok(WorktreeAddOutput {
        path: canonical_target.to_string_lossy().to_string(),
        already_exists: false,
        reattached: false,
    })
}

pub(crate) async fn reattach_worktree(
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
) -> WorktreeResult<WorktreeAddOutput> {
    let db = crate::internal::db::get_db_conn_instance().await;
    let Some(expected_id) = state.entries[index].worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': the registry entry has no persisted worktree id; run \
              `libra worktree repair --confirm` first",
            target.display()
        )));
    };
    let gitdir = target.join(util::ROOT_DIR);
    let current_id = fs::read_to_string(gitdir.join("worktree_id"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if current_id.as_deref() != Some(expected_id.as_str()) {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': its gitdir identity ({}) does not match the registry's \
              persisted id ({expected_id}); run `libra worktree repair --confirm {}` first",
            target.display(),
            current_id.as_deref().unwrap_or("missing"),
            target.display()
        )));
    }
    // The commondir must point at THIS repository's storage: a directory
    // whose (mutable) id happens to match but whose commondir targets
    // another repo must never be re-attached into this one. Missing or
    // corrupt pointers are repair's job, not re-attach's.
    let storage = util::storage_path();
    let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
        })
        .map(|existing| {
            let existing_abs = if existing.is_absolute() {
                existing
            } else {
                gitdir.join(existing)
            };
            fs::canonicalize(&existing_abs).unwrap_or(existing_abs)
                == fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone())
        })
        .unwrap_or(false);
    if !commondir_ok {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': its commondir pointer is missing, corrupt, or targets a \
              different repository's storage; run `libra worktree repair --confirm {}` first",
            target.display(),
            target.display()
        )));
    }

    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "reattach": true,
    });
    let journal_id = journal_append(
        &db,
        WorktreeControl::Add.declare(),
        Some(&expected_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // Publish Active FIRST, then lift the marker: a crash in between leaves
    // an Active entry whose gitdir still carries the marker — repair's
    // reconcile pass removes a stale marker whose entry is Active with a
    // matching id (and the pending journal row rolls the re-attach
    // forward). The reverse order would leave an UNFROZEN detached entry.
    // §C.7 identity check immediately before ACTIVATION. Re-attaching is the
    // one path that turns a dormant claim on an identity back into a live one,
    // so it is where a former collision can be recreated: detach one side,
    // re-add both directories, and without this the second activation restores
    // two active entries at one identity — poisoning every later registry
    // load, including the doctor that would explain it.
    if let Some(identity) = state.entries[index].worktree_id.clone()
        && state.entries.iter().enumerate().any(|(other, entry)| {
            other != index
                && !entry.is_main
                && entry.state.is_active()
                && entry.worktree_id.as_deref() == Some(identity.as_str())
        })
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': identity '{identity}' is already claimed by another ACTIVE \
              worktree. Run `libra worktree doctor`, then \
              `libra worktree repair <path> --resolve-identity --yes` on the one you do not want",
            state.entries[index].path
        )));
    }
    state.entries[index].state = WorktreeEntryState::Active;
    write_state(state)?;
    let marker = gitdir.join(DETACHED_MARKER);
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            // Journal kept: repair finishes lifting the marker.
            return Err(WorktreeError::IoWrite(format!(
                "cannot remove the detached marker '{}' (run `libra worktree repair \
                  --confirm` to finish the re-attach): {error}",
                marker.display()
            )));
        }
    }
    if let Err(error) = lifecycle_delete(&db, &expected_id).await {
        tracing::warn!(
            error,
            "lifecycle row not cleared on re-attach; repair reconciles"
        );
    }
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(
            error,
            "re-attach journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeAddOutput {
        path: target.to_string_lossy().to_string(),
        already_exists: false,
        reattached: true,
    })
}

pub(crate) fn render_add_worktree(
    result: &WorktreeAddOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.add", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.already_exists {
        println!("worktree already exists at {}", result.path);
    } else if result.reattached {
        println!("re-attached detached worktree at {}", result.path);
    } else {
        println!("{}", result.path);
    }
    Ok(())
}

pub(crate) async fn lifecycle_upsert(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
    state: &str,
    path: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let now = chrono::Utc::now().timestamp_millis();
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO worktree_lifecycle (worktree_id, state, path, created_at, updated_at) \
          VALUES (?, ?, ?, ?, ?) \
          ON CONFLICT(worktree_id) DO UPDATE SET state = excluded.state, \
          path = excluded.path, updated_at = excluded.updated_at",
        [
            worktree_id.into(),
            state.into(),
            path.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .map_err(|error| format!("cannot record worktree lifecycle state: {error}"))?;
    Ok(())
}

pub(crate) async fn lifecycle_delete(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM worktree_lifecycle WHERE worktree_id = ?",
        [worktree_id.into()],
    ))
    .await
    .map_err(|error| format!("cannot clear worktree lifecycle state: {error}"))?;
    Ok(())
}

pub(crate) async fn lifecycle_rows(
    db: &sea_orm::DatabaseConnection,
) -> Result<Vec<(String, String)>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT worktree_id, state FROM worktree_lifecycle".to_string(),
        ))
        .await
        .map_err(|error| format!("cannot read the worktree lifecycle mirror: {error}"))?;
    let mut out = Vec::new();
    for row in rows {
        let id: String = row
            .try_get_by_index(0)
            .map_err(|error| format!("corrupt lifecycle row: {error}"))?;
        let state: String = row
            .try_get_by_index(1)
            .map_err(|error| format!("corrupt lifecycle row: {error}"))?;
        out.push((id, state));
    }
    Ok(out)
}

pub(crate) fn probe_path(path: &Path) -> PathPresence {
    match fs::symlink_metadata(path) {
        Ok(_) => PathPresence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathPresence::Missing,
        Err(error) => PathPresence::Unknown(error.to_string()),
    }
}

pub(crate) async fn journal_append(
    db: &sea_orm::DatabaseConnection,
    op: &str,
    worktree_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let now = chrono::Utc::now().timestamp_millis();
    let result = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
              VALUES (?, ?, ?, ?)",
            [
                op.into(),
                worktree_id.into(),
                payload.to_string().into(),
                now.into(),
            ],
        ))
        .await
        .map_err(|error| format!("cannot record the {op} intent journal entry: {error}"))?;
    Ok(result.last_insert_id() as i64)
}

pub(crate) async fn journal_set_stage(
    db: &sea_orm::DatabaseConnection,
    id: i64,
    stage: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE worktree_intent_journal SET stage = ? WHERE id = ?",
        [stage.into(), id.into()],
    ))
    .await
    .map_err(|error| format!("cannot record migration stage {stage}: {error}"))?;
    Ok(())
}

pub(crate) async fn journal_resolve(
    db: &sea_orm::DatabaseConnection,
    id: i64,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM worktree_intent_journal WHERE id = ?",
        [id.into()],
    ))
    .await
    .map_err(|error| format!("cannot resolve intent journal entry {id}: {error}"))?;
    Ok(())
}

pub(crate) async fn journal_pending(
    db: &sea_orm::DatabaseConnection,
) -> Result<Vec<PendingIntent>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT id, op, worktree_id, payload FROM worktree_intent_journal ORDER BY id"
                .to_string(),
        ))
        .await
        .map_err(|error| format!("cannot read the intent journal: {error}"))?;
    let mut pending = Vec::new();
    for row in rows {
        let id: i64 = row
            .try_get_by_index(0)
            .map_err(|error| format!("corrupt intent journal row (id): {error}"))?;
        let op: String = row
            .try_get_by_index(1)
            .map_err(|error| format!("corrupt intent journal row (op): {error}"))?;
        let worktree_id: Option<String> = row
            .try_get_by_index(2)
            .map_err(|error| format!("corrupt intent journal row (worktree_id): {error}"))?;
        let payload_raw: String = row
            .try_get_by_index(3)
            .map_err(|error| format!("corrupt intent journal row (payload): {error}"))?;
        let payload = serde_json::from_str(&payload_raw)
            .map_err(|error| format!("corrupt intent journal payload (id {id}): {error}"))?;
        pending.push(PendingIntent {
            id,
            op,
            worktree_id,
            payload,
        });
    }
    Ok(pending)
}

pub(crate) async fn scoped_state_active(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
) -> bool {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    for table in ["sequence_state", "rebase_state", "bisect_state"] {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE worktree_id = ?");
        match db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                &query,
                [worktree_id.into()],
            ))
            .await
        {
            Ok(Some(row)) => match row.try_get_by_index::<i64>(0) {
                Ok(0) => {}
                Ok(_) => return true,
                Err(_) => return true,
            },
            Ok(None) => {}
            Err(_) => return true,
        }
    }
    false
}

pub(crate) async fn gc_worktree_scoped_rows_strict(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
    directory_gone: bool,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let mut stmts = vec![
        "DELETE FROM reference WHERE worktree_id = ? AND kind = 'Head'",
        "DELETE FROM reflog WHERE worktree_id = ?",
        "DELETE FROM sequence_state WHERE worktree_id = ?",
        "DELETE FROM rebase_state WHERE worktree_id = ?",
        "DELETE FROM working_dirty WHERE worktree_id = ?",
        "DELETE FROM working_dirty_meta WHERE worktree_id = ?",
    ];
    // Layer registrations/ownership and the sparse view (W1 §C.4.1.1):
    // purged ONLY when the worktree directory is actually gone
    // (`--delete-dir`, prune, or it had already vanished). A default
    // `remove` RETAINS the directory — and a retained `.libra` still
    // operates as a repository — so its layer ownership rows must survive
    // to keep the still-materialized overlay files un-stageable
    // (never-enters-commit), and its sparse view keeps filtering that
    // directory's queries. The retained directory cannot be re-registered
    // while non-empty (`worktree add` refuses), so the rows guard it until
    // the directory is cleared; orphaned rows are then reclaimed by the W3
    // worktree doctor (they are invisible to every live scope meanwhile).
    if directory_gone {
        stmts.push("DELETE FROM layer WHERE worktree_id = ?");
        stmts.push("DELETE FROM layer_path WHERE worktree_id = ?");
        stmts.push("DELETE FROM sparse_view WHERE worktree_id = ?");
        stmts.push("DELETE FROM sparse_view_meta WHERE worktree_id = ?");
    }
    // `bisect_state` is owned by migration `2026072301`, but bare or
    // pre-migration test databases may still lack it — only purge when the
    // table exists (a DELETE on a missing table would log a spurious warn).
    let has_bisect_table = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'bisect_state'",
        ))
        .await
        .ok()
        .flatten()
        .is_some();
    if has_bisect_table {
        stmts.push("DELETE FROM bisect_state WHERE worktree_id = ?");
    }
    for sql in stmts {
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            sql,
            [worktree_id.into()],
        ))
        .await
        .map_err(|e| format!("{sql}: {e}"))?;
    }
    Ok(())
}

pub(crate) fn create_worktree_gitdir(
    common_storage: &Path,
    gitdir: &Path,
    worktree_id: &str,
) -> io::Result<()> {
    fs::create_dir_all(gitdir)?;
    fs::write(
        gitdir.join("commondir"),
        format!("{}\n", common_storage.display()),
    )?;
    fs::write(gitdir.join("worktree_id"), format!("{worktree_id}\n"))?;
    Ok(())
}

pub(crate) fn remove_worktree_storage_link(link_path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(link_path)?;
    if metadata.file_type().is_symlink() {
        return fs::remove_file(link_path);
    }
    if metadata.is_dir() {
        return fs::remove_dir_all(link_path);
    }
    fs::remove_file(link_path)
}

pub(crate) fn run_list_worktrees() -> WorktreeResult<WorktreeListOutput> {
    run_list_worktrees_at(&state_path())
}

pub(crate) fn run_list_worktrees_at(
    registry_path: &std::path::Path,
) -> WorktreeResult<WorktreeListOutput> {
    let state = load_state_readonly_at(registry_path)?;
    let worktrees = state
        .entries
        .into_iter()
        .map(|w| {
            // v2: prefer the registry's PERSISTED stable id; fall back to
            // the gitdir/synthesis probe for rows the upgrade could not
            // backfill.
            let worktree_id = w
                .worktree_id
                .clone()
                .or_else(|| resolve_entry_worktree_id(&w.path, w.is_main));
            let layout = detect_entry_layout(Path::new(&w.path), w.is_main);
            WorktreeListEntry {
                kind: if w.is_main { "main" } else { "worktree" },
                exists: Path::new(&w.path).exists(),
                worktree_id,
                state: w.state.as_str(),
                layout,
                epoch: w.epoch,
                path: w.path,
                is_main: w.is_main,
                locked: w.locked,
                lock_reason: w.lock_reason,
            }
        })
        .collect();
    Ok(WorktreeListOutput { worktrees })
}

pub(crate) fn detect_entry_layout(path: &Path, is_main: bool) -> &'static str {
    if is_main {
        return "main";
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return "missing",
        Err(_) => return "corrupt",
        Ok(_) => {}
    }
    let gitdir = path.join(util::ROOT_DIR);
    match fs::symlink_metadata(&gitdir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => "corrupt",
        Err(_) => "corrupt",
        Ok(meta) if meta.file_type().is_symlink() => {
            let storage = util::storage_path();
            let resolved = fs::canonicalize(&gitdir).ok();
            let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
            if resolved.as_deref() == Some(canonical_storage.as_path()) {
                "legacy-symlink"
            } else {
                "corrupt"
            }
        }
        Ok(meta) if meta.is_dir() => {
            let storage = util::storage_path();
            let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
            let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
                .map(|p| {
                    let abs = if p.is_absolute() { p } else { gitdir.join(p) };
                    fs::canonicalize(&abs).unwrap_or(abs)
                })
                .is_some_and(|p| p == canonical_storage);
            if commondir_ok || gitdir.join(DETACHED_MARKER).exists() {
                "linked-v2"
            } else {
                "corrupt"
            }
        }
        Ok(_) => "corrupt",
    }
}

pub(crate) fn resolve_entry_worktree_id(path: &str, is_main: bool) -> Option<String> {
    if is_main {
        return None;
    }
    let gitdir = Path::new(path).join(util::ROOT_DIR);
    if let Ok(id) = fs::read_to_string(gitdir.join("worktree_id")) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    Some(util::worktree_instance_id(&canonical))
}

pub(crate) async fn format_worktree_porcelain(
    worktrees: &[WorktreeListEntry],
) -> Result<String, crate::internal::branch::BranchStoreError> {
    let mut out = String::new();
    for w in worktrees {
        out.push_str("worktree ");
        out.push_str(&w.path);
        out.push('\n');
        match Head::head_for_worktree_scope(w.worktree_id.as_deref()).await {
            Ok(Some((head, commit))) => {
                if let Some(sha) = commit {
                    out.push_str(&format!("HEAD {sha}\n"));
                }
                match head {
                    Head::Branch(name) => {
                        let full = if name.starts_with("refs/") {
                            name
                        } else {
                            format!("refs/heads/{name}")
                        };
                        out.push_str(&format!("branch {full}\n"));
                    }
                    Head::Detached(_) => out.push_str("detached\n"),
                }
            }
            // No HEAD row for this scope (legacy layout / missing): omit HEAD
            // lines deterministically rather than stamping a wrong commit.
            Ok(None) => {}
            // A CORRUPT row (unparseable id, or one whose hash algorithm is
            // not this repository's) is a different thing entirely: swallowing
            // it printed a successful listing with the HEAD lines silently
            // absent. Fail closed with the store's own message.
            Err(error) => return Err(error),
        }
        if w.locked {
            match w.lock_reason.as_deref() {
                Some(reason) if !reason.is_empty() => out.push_str(&format!("locked {reason}\n")),
                _ => out.push_str("locked\n"),
            }
        }
        // W3-s3 (§C.6.1): versioned layout line — additive to the frozen
        // porcelain attributes (declared in COMPATIBILITY/docs).
        out.push_str(&format!("layout {}\n", w.layout));
        out.push('\n');
    }
    Ok(out)
}

pub(crate) fn info_file_has_effective_content(
    path: &std::path::Path,
    name: &str,
    base: &Path,
) -> bool {
    match name {
        "exclude" => crate::utils::util::ignore_file_defines_any_pattern(path, base),
        "attributes" => crate::utils::attributes::file_defines_any_rule(path, base),
        // Unreachable for WORKTREE_INFO_FILE_NAMES; fail closed (report it)
        // rather than silently hiding an unknown info source.
        _ => path.is_file(),
    }
}

pub(crate) fn adopt_common_info_files(target: &str) -> CliResult<String> {
    let common = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve the repository common storage: {error}"
        ))
    })?;
    let target_path = std::path::PathBuf::from(target);
    let target_gitdir = target_path.join(crate::utils::util::ROOT_DIR);
    // No-follow, and only a definitive NotFound means "not linked": a
    // dangling or uninspectable commondir marker must reach the resolver
    // below, whose corruption message names the real problem rather than
    // claiming the path is not a worktree at all.
    if matches!(
        std::fs::symlink_metadata(target_gitdir.join("commondir")),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Err(CliError::command_usage(format!(
            "'{target}' is not a linked worktree of this repository (no `.libra/commondir`); \
              --adopt-info-to copies INTO a linked worktree's own gitdir"
        )));
    }
    let target_common = crate::utils::util::try_get_storage_path(Some(target_path.clone()))
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot resolve '{target}' as a worktree of this repository: {error}"
            ))
        })?;
    if target_common != common {
        return Err(CliError::command_usage(format!(
            "'{target}' belongs to a DIFFERENT repository (its common storage is '{}'); \
              refusing to copy this repository's info files there",
            target_common.display()
        )));
    }
    let mut copied = Vec::new();
    let mut skipped = Vec::new();
    for name in WORKTREE_INFO_FILE_NAMES {
        let source = common.join("info").join(name);
        let mut reader = match std::fs::File::open(&source) {
            Ok(reader) => reader,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot read '{}': {error}",
                    source.display()
                )));
            }
        };
        let destination_dir = target_gitdir.join("info");
        std::fs::create_dir_all(&destination_dir).map_err(|error| {
            CliError::fatal(format!(
                "cannot create '{}': {error}",
                destination_dir.display()
            ))
        })?;
        let destination = destination_dir.join(name);
        // FAILURE-ATOMIC, NO-OVERWRITE publish: STREAM the source into a
        // same-directory temp file (bounded memory; no partial file can
        // ever sit at the final name), then `hard_link` it into place —
        // link(2) fails with AlreadyExists atomically and never follows a
        // symlink at the destination, so a concurrent creator's
        // worktree-local policy can neither be truncated nor redirected,
        // and a crash mid-copy leaves only a temp file the next run
        // removes.
        let temp_path = destination_dir.join(format!(".{name}.adopt-tmp-{}", std::process::id()));
        let publish = (|| -> std::io::Result<bool> {
            let mut temp = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            std::io::copy(&mut reader, &mut temp)?;
            temp.sync_all()?;
            drop(temp);
            match std::fs::hard_link(&temp_path, &destination) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            }
        })();
        // The temp file never survives, success or failure.
        let _ = std::fs::remove_file(&temp_path);
        match publish {
            Ok(true) => copied.push(format!("info/{name}")),
            Ok(false) => skipped.push(format!("info/{name} (destination already exists)")),
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot adopt '{}' into '{}': {error}",
                    source.display(),
                    destination.display()
                )));
            }
        }
    }
    if copied.is_empty() && skipped.is_empty() {
        return Ok(
            "nothing to adopt: the common storage has no info/exclude or \
                    info/attributes"
                .to_string(),
        );
    }
    let mut lines = Vec::new();
    if !copied.is_empty() {
        lines.push(format!("adopted into '{target}': {}", copied.join(", ")));
    }
    if !skipped.is_empty() {
        lines.push(format!("skipped: {}", skipped.join(", ")));
    }
    Ok(lines.join("\n"))
}

pub(crate) fn clear_common_info_files() -> CliResult<String> {
    let common = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve the repository common storage: {error}"
        ))
    })?;
    let mut removed = Vec::new();
    for name in WORKTREE_INFO_FILE_NAMES {
        let path = common.join("info").join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(format!("info/{name}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot remove '{}': {error}",
                    path.display()
                )));
            }
        }
    }
    if removed.is_empty() {
        Ok(
            "nothing to clear: the common storage has no info/exclude or info/attributes"
                .to_string(),
        )
    } else {
        Ok(format!(
            "cleared from common storage: {}",
            removed.join(", ")
        ))
    }
}

pub(crate) async fn lock_worktree(
    path: String,
    reason: Option<String>,
) -> WorktreeResult<WorktreeLockOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry = match find_entry_mut(&mut state, &target) {
        Some(e) => e,
        None => return Err(WorktreeError::NoSuchWorktree { path }),
    };
    if entry.locked {
        return Ok(WorktreeLockOutput {
            path: target.to_string_lossy().to_string(),
            locked: true,
            lock_reason: entry.lock_reason.clone(),
            changed: false,
        });
    }
    entry.locked = true;
    entry.lock_reason = reason;
    let lock_reason = entry.lock_reason.clone();
    write_state(&state)?;
    Ok(WorktreeLockOutput {
        path: target.to_string_lossy().to_string(),
        locked: true,
        lock_reason,
        changed: true,
    })
}

pub(crate) fn render_lock_worktree(
    result: &WorktreeLockOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.lock", result, output);
    }
    Ok(())
}

pub(crate) async fn unlock_worktree(path: String) -> WorktreeResult<WorktreeUnlockOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry = match find_entry_mut(&mut state, &target) {
        Some(e) => e,
        None => return Err(WorktreeError::NoSuchWorktree { path }),
    };
    if !entry.locked {
        return Ok(WorktreeUnlockOutput {
            path: target.to_string_lossy().to_string(),
            locked: false,
            changed: false,
        });
    }
    entry.locked = false;
    entry.lock_reason = None;
    write_state(&state)?;
    Ok(WorktreeUnlockOutput {
        path: target.to_string_lossy().to_string(),
        locked: false,
        changed: true,
    })
}

pub(crate) fn render_unlock_worktree(
    result: &WorktreeUnlockOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.unlock", result, output);
    }
    Ok(())
}

pub(crate) async fn move_worktree(src: String, dest: String) -> WorktreeResult<WorktreeMoveOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let src_path = resolve_path(&src, "source worktree path")?;
    let dest_path = resolve_path(&dest, "destination worktree path")?;
    let storage = util::storage_path();

    if util::is_sub_path(&dest_path, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "destination cannot be inside .libra storage: {}",
            dest_path.display()
        )));
    }

    if find_entry(&state, &dest_path).is_some() {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination already registered as worktree: {}",
            dest_path.display()
        )));
    }

    let index = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == src_path)
        .ok_or(WorktreeError::NoSuchWorktree { path: src })?;

    if state.entries[index].is_main {
        return Err(WorktreeError::MainWorktree {
            action: "move",
            path: src_path.to_string_lossy().to_string(),
        });
    }
    if state.entries[index].locked {
        return Err(WorktreeError::LockedWorktree {
            action: "move",
            path: src_path.to_string_lossy().to_string(),
        });
    }

    if dest_path.exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination already exists: {}",
            dest_path.display()
        )));
    }

    // Durable intent BEFORE the first cross-medium mutation (§C.7): a crash
    // between the registry write and the directory rename is rolled forward
    // (or back) by `worktree repair` from this record.
    let journal_worktree_id = state.entries[index]
        .worktree_id
        .clone()
        .or_else(|| resolve_worktree_id(&src_path));
    // W3-s3: moving a worktree with an UNFINISHED layout migration would
    // strand its journal at the old path and let reconciliation unfreeze
    // the relocated (still-unmigrated) state — refuse until repair settles
    // it.
    {
        let db = crate::internal::db::get_db_conn_instance().await;
        let pending = journal_pending(&db)
            .await
            .map_err(WorktreeError::OperationBlocked)?;
        if pending.iter().any(|intent| {
            intent.op == "migrate"
                && (intent.worktree_id.as_deref() == journal_worktree_id.as_deref()
                    || intent.payload["path"].as_str() == Some(src_path.to_string_lossy().as_ref()))
        }) {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' has an unfinished layout migration; run `libra worktree repair \
                  --confirm` first",
                src_path.display()
            )));
        }
    }
    let payload = serde_json::json!({
        "src": src_path.to_string_lossy(),
        "dest": dest_path.to_string_lossy(),
    });
    let db = crate::internal::db::get_db_conn_instance().await;
    let journal_id = journal_append(
        &db,
        WorktreeControl::Move.declare(),
        journal_worktree_id.as_deref(),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    let old_path = state.entries[index].path.clone();
    state.entries[index].path = dest_path.to_string_lossy().to_string();
    if let Err(e) = write_state(&state) {
        state.entries[index].path = old_path;
        let _ = journal_resolve(&db, journal_id).await;
        return Err(e);
    }

    let move_result = fs::rename(&src_path, &dest_path).or_else(|error| {
        if error.kind() != io::ErrorKind::CrossesDevices {
            return Err(error);
        }

        // `rename(2)` cannot cross filesystems. Copy first, then remove the
        // source only after the destination is complete; this preserves the
        // worktree if either phase fails.
        let mut options = fs_extra::dir::CopyOptions::new();
        options.overwrite = false;
        options.copy_inside = false;
        let destination_parent = dest_path.parent().unwrap_or_else(|| Path::new("."));
        fs_extra::dir::copy(&src_path, destination_parent, &options)
            .map_err(|copy_error| io::Error::other(copy_error.to_string()))?;
        let copied_path = destination_parent.join(
            src_path
                .file_name()
                .ok_or_else(|| io::Error::other("worktree source has no directory name"))?,
        );
        if copied_path != dest_path {
            fs::rename(&copied_path, &dest_path)?;
        }
        if let Err(remove_error) = fs::remove_dir_all(&src_path) {
            let _ = fs::remove_dir_all(&dest_path);
            return Err(remove_error);
        }
        Ok(())
    });
    if let Err(e) = move_result {
        state.entries[index].path = old_path;
        write_state(&state)?;
        let _ = journal_resolve(&db, journal_id).await;
        return Err(WorktreeError::IoWrite(format!(
            "failed to move worktree directory '{}' to '{}': {e}",
            src_path.display(),
            dest_path.display()
        )));
    }
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(
            error,
            "move journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeMoveOutput {
        source: src_path.to_string_lossy().to_string(),
        destination: dest_path.to_string_lossy().to_string(),
        registry_updated: true,
        disk_directory_moved: true,
    })
}

pub(crate) fn render_move_worktree(
    result: &WorktreeMoveOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.move", result, output);
    }
    Ok(())
}

pub(crate) async fn prune_worktrees() -> WorktreeResult<WorktreePruneOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let _repository_ref_lease = acquire_worktree_ref_lease().await?;
    let mut state = load_state()?;

    // §C.7: prune only handles entries whose path is PROVEN missing
    // (NotFound). Any other stat error — permissions, an unmounted volume —
    // must NOT classify the worktree as missing; those entries are kept.
    fn path_proven_missing(path: &Path) -> bool {
        matches!(
            fs::symlink_metadata(path),
            Err(ref error) if error.kind() == io::ErrorKind::NotFound
        )
    }

    let mut to_prune: Vec<(String, Option<String>)> = Vec::new();
    for entry in &state.entries {
        if entry.is_main || entry.locked || entry.state == WorktreeEntryState::Tombstone {
            // Tombstones are repair's job (the directory is already gone by
            // definition; only the scoped cleanup is pending).
            continue;
        }
        if !path_proven_missing(Path::new(&entry.path)) {
            continue;
        }
        let id = entry
            .worktree_id
            .clone()
            .or_else(|| resolve_worktree_id(Path::new(&entry.path)));
        to_prune.push((entry.path.clone(), id));
    }

    let mut pruned: Vec<String> = Vec::new();
    let mut tombstoned: Vec<String> = Vec::new();
    if !to_prune.is_empty() {
        let db = crate::internal::db::get_db_conn_instance().await;
        // An entry with ACTIVE sequencer/bisect state is never pruned —
        // its rows anchor the interrupted operation's objects.
        let mut eligible: Vec<(String, Option<String>)> = Vec::new();
        for (path, id) in to_prune {
            if let Some(id_str) = id.as_deref()
                && scoped_state_active(&db, id_str).await
            {
                continue;
            }
            // §C.7: an entry an agent still holds a LIVE LEASE on is never
            // pruned either — the path is gone but the WORKSPACE is not (an
            // in-flight re-provision, or a directory another actor removed
            // out of band), and deleting its scoped rows would pull the
            // workspace out from under a fenced owner. Queried BY WORKTREE
            // ID: the path-keyed lookup canonicalizes its query and cannot
            // match a path that no longer exists — which is every prune
            // candidate.
            if let Some(id_str) = id.as_deref() {
                match crate::internal::workspace::WorkspaceStore::find_live_linked_with_conn(
                    &db, id_str,
                )
                .await
                {
                    Ok(Some(record)) => {
                        let now = chrono::Utc::now().timestamp_millis();
                        if record.lease_expires_at.is_some_and(|expiry| expiry > now) {
                            // A live, UNEXPIRED lease: never pruned.
                            continue;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        // FAIL CLOSED: an unreadable workspace store keeps
                        // the entry rather than pruning blind.
                        continue;
                    }
                }
            }
            eligible.push((path, id));
        }
        if !eligible.is_empty() {
            let payload = serde_json::json!({
                "paths": eligible.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            });
            let journal_id = journal_append(&db, WorktreeControl::Prune.declare(), None, &payload)
                .await
                .map_err(WorktreeError::OperationBlocked)?;
            let mut mirror_failed = false;
            for (path, id) in &eligible {
                // STRICT per-entry cleanup: a GC failure keeps the entry as
                // a TOMBSTONE (the directory is proven missing) so the rows
                // stay visible to repair and the down-migration guard —
                // never silently orphaned.
                let cleaned = if let Some(id_str) = id.as_deref() {
                    match gc_worktree_scoped_rows_strict(&db, id_str, true).await {
                        Ok(()) => {
                            let _ = lifecycle_delete(&db, id_str).await;
                            true
                        }
                        Err(error) => {
                            tracing::warn!(
                                worktree_id = id_str,
                                error,
                                "prune cleanup failed; keeping a tombstone"
                            );
                            if let Err(mirror_error) = lifecycle_upsert(
                                &db,
                                id_str,
                                WorktreeEntryState::Tombstone.as_str(),
                                path,
                            )
                            .await
                            {
                                // Without the mirror row the down guard
                                // cannot see this tombstone: keep the
                                // journal row pending so repair retries.
                                tracing::warn!(
                                    worktree_id = id_str,
                                    error = mirror_error,
                                    "tombstone mirror write failed; journal kept for repair"
                                );
                                mirror_failed = true;
                            }
                            false
                        }
                    }
                } else {
                    true
                };
                if cleaned {
                    pruned.push(path.clone());
                } else {
                    tombstoned.push(path.clone());
                }
            }
            let pruned_set: std::collections::HashSet<&String> = pruned.iter().collect();
            for entry in &mut state.entries {
                if tombstoned.contains(&entry.path) {
                    entry.state = WorktreeEntryState::Tombstone;
                }
            }
            state.entries.retain(|w| !pruned_set.contains(&w.path));
            write_state(&state)?;
            if mirror_failed {
                tracing::warn!(
                    "prune left a tombstone whose mirror write failed; the journal row \
                      stays pending for `worktree repair`"
                );
            } else if let Err(error) = journal_resolve(&db, journal_id).await {
                tracing::warn!(
                    error,
                    "prune journal entry not resolved; repair will reconcile"
                );
            }
        }
    }

    Ok(WorktreePruneOutput {
        pruned_count: pruned.len(),
        pruned,
        tombstoned,
    })
}

pub(crate) fn render_prune_worktrees(
    result: &WorktreePruneOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.prune", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.pruned.is_empty() {
        println!("No worktrees to prune");
        return Ok(());
    }
    println!("Will prune {} worktrees:", result.pruned_count);
    for path in &result.pruned {
        println!("  {}", path);
    }
    println!("Pruned {} worktrees", result.pruned_count);
    Ok(())
}

pub(crate) async fn remove_worktree(
    path: String,
    delete_dir: bool,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let _repository_ref_lease = acquire_worktree_ref_lease().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;

    let index = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == target)
        .ok_or(WorktreeError::NoSuchWorktree { path })?;

    let entry = &state.entries[index];
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: "remove",
            path: target.to_string_lossy().to_string(),
        });
    }
    if entry.locked {
        return Err(WorktreeError::LockedWorktree {
            action: "remove",
            path: target.to_string_lossy().to_string(),
        });
    }
    // W3-s3: a LEGACY symlink target shares main's `.libra` — any lifecycle
    // marker written into it would land in MAIN storage and freeze the main
    // repository. Refuse until the layout migration settles it.
    if detect_entry_layout(&target, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' uses the legacy shared-.libra symlink layout; run `libra worktree \
              repair --migrate-layout --confirm {}` first",
            target.display(),
            target.display()
        )));
    }
    // §C.7: a worktree an AGENT holds a live workspace lease on is not the
    // human's to detach or delete out from under it — the lease's fenced
    // owner is mid-work there. Release or let the lease lapse first.
    {
        let db = crate::internal::db::get_db_conn_instance().await;
        // FAIL CLOSED on a store that cannot be read: swallowing the error
        // would disable this protection exactly when the leased agent is
        // busy writing the same database.
        let lease_error = |error: crate::internal::workspace::WorkspaceError| {
            WorktreeError::OperationBlocked(format!(
                "cannot verify agent workspace leases for '{}': {error}; refusing to \
                  remove until the workspace store is readable",
                target.display()
            ))
        };
        let by_path =
            crate::internal::workspace::WorkspaceStore::find_live_by_path_with_conn(&db, &target)
                .await
                .map_err(lease_error)?;
        let by_id = match entry.worktree_id.as_deref() {
            Some(id) => {
                crate::internal::workspace::WorkspaceStore::find_live_linked_with_conn(&db, id)
                    .await
                    .map_err(lease_error)?
            }
            None => None,
        };
        // An EXPIRED lease does not block: nothing may run the scavenger for
        // a long time, and "let it expire" must actually unblock the human
        // (§C.7 acceptance: a crashed agent's worktree stays recoverable).
        let now = chrono::Utc::now().timestamp_millis();
        let unexpired = |record: &crate::internal::workspace::WorkspaceRecord| {
            record.lease_expires_at.is_some_and(|expiry| expiry > now)
        };
        if let Some(record) = by_path.into_iter().chain(by_id).find(unexpired) {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' is held by live agent workspace '{}' (lease fence {}); release the \
                  lease or let it expire before removing the worktree",
                target.display(),
                record.workspace_id,
                record.lease_fence
            )));
        }
    }
    let entry_state = entry.state;
    if entry_state == WorktreeEntryState::Tombstone {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is a tombstone (directory already deleted, scoped cleanup pending); run \
              `libra worktree repair --confirm` to retry the cleanup",
            target.display()
        )));
    }
    if !delete_dir && entry_state == WorktreeEntryState::DetachedFromRegistry {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is already detached from the registry; re-add it or use --delete-dir",
            target.display()
        )));
    }

    // The registry's PERSISTED id is authoritative (v2); the gitdir probe is
    // only a fallback for pre-v2 rows the loader could not backfill.
    let worktree_id_for_gc = state.entries[index]
        .worktree_id
        .clone()
        .or_else(|| resolve_worktree_id(&target));

    // Resolve the pooled connection ONCE, while the cwd-based path lookup
    // is still valid — later steps may write the detached marker, after
    // which any cwd-based storage resolution inside the target would fail.
    let db = crate::internal::db::get_db_conn_instance().await;

    // §C.7: a worktree with ACTIVE sequencer/rebase/bisect state refuses
    // both remove modes — detaching would strand a half-finished operation
    // behind the fail-closed gate, deleting would destroy it.
    if let Some(id) = worktree_id_for_gc.as_deref()
        && scoped_state_active(&db, id).await
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has an in-progress rebase/cherry-pick/bisect; finish or abort it there \
              first",
            target.display()
        )));
    }
    // …and the FILE-backed half of the same rule: merge/revert sidecars and a
    // held autostash live in the target's gitdir, not in DB rows.
    refuse_active_sidecar_state(&target.join(util::ROOT_DIR), "removing the worktree")?;

    if delete_dir {
        remove_worktree_delete_dir(
            &db,
            &mut state,
            index,
            &target,
            worktree_id_for_gc,
            entry_state,
        )
        .await
    } else {
        remove_worktree_detach(&db, &mut state, index, &target, worktree_id_for_gc).await
    }
}

pub(crate) async fn remove_worktree_detach(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let Some(worktree_id) = worktree_id else {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot detach '{}': its stable worktree id is unknown; run `libra worktree \
              repair --confirm` first",
            target.display()
        )));
    };
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": false,
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::Remove.declare(),
        Some(&worktree_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // Recovery-ordered: lifecycle mirror → registry state → journal → the
    // gitdir marker LAST. The marker fail-closes every storage resolution
    // inside the target directory, so all cwd-sensitive work (DB access,
    // registry writes — `remove .` runs with cwd IN the target) must finish
    // before it appears. A crash before the marker leaves the journal row
    // (or the detached entry) for `worktree repair`, whose reconcile pass
    // rewrites missing markers from the registry.
    lifecycle_upsert(
        db,
        &worktree_id,
        WorktreeEntryState::DetachedFromRegistry.as_str(),
        &target.to_string_lossy(),
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    state.entries[index].state = WorktreeEntryState::DetachedFromRegistry;
    write_state(state)?;
    // Marker BEFORE resolving the journal: if the marker write fails, the
    // pending row makes `worktree repair` re-freeze the directory. The DB
    // handle was resolved before the marker exists, so the resolve below
    // cannot trip the gate even when cwd is inside the target.
    write_detached_marker(target, &worktree_id)?;
    if let Err(error) = journal_resolve(db, journal_id).await {
        tracing::warn!(
            error,
            "detach journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeRemoveOutput {
        path: target.to_string_lossy().into_owned(),
        registry_removed: false,
        disk_directory_deleted: false,
        detached: true,
        tombstone: false,
    })
}

pub(crate) async fn remove_worktree_delete_dir(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
    entry_state: WorktreeEntryState,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": true,
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::Remove.declare(),
        worktree_id.as_deref(),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // A DETACHED worktree's marker fail-closes the in-worktree dirty check;
    // lift it for the check and restore it on refusal. Under the registry
    // lock, and journaled, so a crash mid-window is rolled forward by
    // repair (the registry entry still says detached).
    let _marker_lifted = if entry_state == WorktreeEntryState::DetachedFromRegistry {
        let marker = target.join(util::ROOT_DIR).join(DETACHED_MARKER);
        match fs::remove_file(&marker) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                let _ = journal_resolve(db, journal_id).await;
                return Err(WorktreeError::IoWrite(format!(
                    "cannot lift the detached marker for the dirty check: {error}"
                )));
            }
        }
    } else {
        false
    };

    let dirty = worktree_is_dirty(target).await;
    match dirty {
        Ok(false) => {}
        Ok(true) => {
            let restored = restore_marker_on_refusal(
                entry_state == WorktreeEntryState::DetachedFromRegistry,
                target,
                worktree_id.as_deref(),
            );
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(WorktreeError::DirtyWorktree {
                path: target.to_string_lossy().to_string(),
            });
        }
        Err(error) => {
            let restored = restore_marker_on_refusal(
                entry_state == WorktreeEntryState::DetachedFromRegistry,
                target,
                worktree_id.as_deref(),
            );
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(error);
        }
    }

    {
        // `remove --delete-dir .` runs with cwd INSIDE the target: move out
        // before deleting it, or every later cwd-based lookup (and the user's
        // shell) sits in a deleted directory.
        // Test commands run in one process, so this exceptional cwd repair must
        // participate in the same lock as ChangeDirGuard.
        #[cfg(test)]
        let _cwd_lock = crate::utils::test::cwd_lock_guard();
        if let Ok(cwd) = env::current_dir()
            && cwd.starts_with(target)
            && let Some(parent) = target.parent()
        {
            let _ = env::set_current_dir(parent);
        }
    }
    if let Err(e) = fs::remove_dir_all(target) {
        // Re-freeze a detached entry before surfacing the error — the
        // journal row stays pending either way (no resolve on this path),
        // so repair re-establishes whatever this restore could not.
        let _ = restore_marker_on_refusal(
            entry_state == WorktreeEntryState::DetachedFromRegistry,
            target,
            worktree_id.as_deref(),
        );
        return Err(WorktreeError::IoWrite(format!(
            "failed to delete worktree directory '{}': {e}",
            target.display()
        )));
    }
    // The tombstone contract says "directory DURABLY deleted" — so a real
    // fsync failure keeps the entry as a tombstone (rows preserved, repair
    // retries) rather than proceeding to destroy the scoped rows on the
    // strength of an unflushed unlink. Platforms whose directory handles
    // refuse sync (an io::ErrorKind::Unsupported open/sync) keep the old
    // best-effort behavior.
    let durable = match target.parent().map(fs::File::open) {
        Some(Ok(dir)) => match dir.sync_all() {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => true,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "cannot fsync the deleted worktree's parent; keeping a tombstone"
                );
                false
            }
        },
        // An unopenable parent cannot prove durability either way — keep
        // the historical best-effort answer rather than tombstoning every
        // removal on exotic filesystems.
        _ => true,
    };

    let cleanup_failed = if !durable {
        true
    } else if let Some(id) = worktree_id.as_deref() {
        match gc_worktree_scoped_rows_strict(db, id, true).await {
            Ok(()) => {
                let _ = lifecycle_delete(db, id).await;
                false
            }
            Err(error) => {
                tracing::warn!(
                    worktree_id = id,
                    error,
                    "scoped-row cleanup failed after directory deletion; keeping a tombstone"
                );
                lifecycle_upsert(
                    db,
                    id,
                    WorktreeEntryState::Tombstone.as_str(),
                    &target.to_string_lossy(),
                )
                .await
                .map_err(WorktreeError::OperationBlocked)?;
                true
            }
        }
    } else {
        false
    };

    if cleanup_failed {
        state.entries[index].state = WorktreeEntryState::Tombstone;
    } else {
        state.entries.remove(index);
    }
    write_state(state)?;
    if let Err(error) = journal_resolve(db, journal_id).await {
        tracing::warn!(
            error,
            "remove journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeRemoveOutput {
        path: target.to_string_lossy().into_owned(),
        registry_removed: !cleanup_failed,
        disk_directory_deleted: true,
        detached: false,
        tombstone: cleanup_failed,
    })
}

pub(crate) fn render_remove_worktree(
    result: &WorktreeRemoveOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.remove", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.tombstone {
        println!(
            "Deleted worktree directory '{}', but the scoped-state cleanup failed — a \
              tombstone entry remains; run `libra worktree repair --confirm` to retry.",
            result.path
        );
    } else if result.disk_directory_deleted {
        println!(
            "Removed worktree '{}' from registry and deleted directory.",
            result.path
        );
    } else {
        println!(
            "Detached worktree '{}' from the registry. Directory and its state kept on \
              disk (frozen); re-add it with `libra worktree add` or delete it with \
              `--delete-dir`.",
            result.path
        );
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn umount_fuse_path(
    path: String,
    cleanup: bool,
) -> WorktreeResult<WorktreeUmountOutput> {
    let target = resolve_path(&path, "FUSE worktree path")?;
    let mountpoint = fuse_utils::resolve_task_worktree_mountpoint_arg(&target);
    fuse_utils::force_unmount_path(&mountpoint).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "failed to unmount FUSE path {}: {source}",
            mountpoint.display()
        ))
    })?;

    let mut cleanup_root = None;
    let mut cleanup_root_removed = false;
    if cleanup {
        let root = fuse_utils::fuse_task_worktree_cleanup_root(&mountpoint).ok_or_else(|| {
            WorktreeError::InvalidTarget(format!(
                "--cleanup only supports Libra task FUSE worktree paths ending in '/workspace': {}",
                mountpoint.display()
            ))
        })?;
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|source| {
                WorktreeError::IoWrite(format!(
                    "failed to remove FUSE worktree root '{}': {source}",
                    root.display()
                ))
            })?;
            cleanup_root_removed = true;
        }
        cleanup_root = Some(root.to_string_lossy().to_string());
    }

    Ok(WorktreeUmountOutput {
        mountpoint: mountpoint.to_string_lossy().to_string(),
        unmounted: true,
        cleanup_requested: cleanup,
        cleanup_root,
        cleanup_root_removed,
    })
}

#[cfg(unix)]
pub(crate) fn render_umount_fuse_path(
    result: &WorktreeUmountOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.umount", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("unmounted {}", result.mountpoint);
    if let Some(cleanup_root) = &result.cleanup_root {
        println!("removed {}", cleanup_root);
    }
    Ok(())
}
