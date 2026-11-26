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
/// The caller holds the common registry lock through resolution and publication.
pub(crate) async fn add_worktree_locked(
    name: String,
    path: Option<String>,
    target_spec: Option<String>,
    detach: bool,
    new_branch: Option<String>,
    force: bool,
) -> WorktreeResult<WorktreeAddOutput> {
    use crate::utils::worktree_layout;
    worktree_layout::validate_name(&name).map_err(|error| {
        WorktreeError::InvalidTarget(format!("invalid worktree name '{name}': {error}; use `libra worktree add <name> --path <path>` with a valid name to specify a filesystem destination"))
    })?;
    let storage = worktree_layout::current_common_storage().map_err(|error| {
        WorktreeError::IoRead(format!("cannot resolve common worktree storage: {error}"))
    })?;
    let locations = registered_worktree_locations()?;
    let named = locations
        .iter()
        .find(|entry| entry.name.as_deref() == Some(name.as_str()));
    if named.is_some_and(|entry| entry.is_fuse) {
        return Err(WorktreeError::OperationBlocked(format!(
            "worktree name '{name}' belongs to a FUSE worktree; use its existing backend"
        )));
    }
    let requested = path
        .as_deref()
        .map(|path| resolve_path(path, "worktree destination"))
        .transpose()?;
    let target = match (named, requested) {
        (Some(entry), Some(requested)) if entry.path != requested => {
            return Err(WorktreeError::OperationBlocked(format!(
                "worktree name '{name}' is already registered at '{}'; use worktree move to change its path",
                entry.path.display()
            )));
        }
        (Some(entry), _) => entry.path.clone(),
        (None, Some(requested)) => requested,
        (None, None) => worktree_layout::linked_root(&storage, &name).map_err(|error| {
            WorktreeError::InvalidTarget(format!(
                "cannot resolve the default worktree destination: {error}"
            ))
        })?,
    };
    worktree_layout::validate_linked_destination(&storage, &name, &target).map_err(|error| {
        WorktreeError::InvalidTarget(format!(
            "invalid worktree destination '{}': {error}",
            target.display()
        ))
    })?;
    if let Some(existing) = locations.iter().find(|entry| entry.path == target)
        && (existing.is_fuse
            || existing
                .name
                .as_deref()
                .is_some_and(|existing| existing != name)
            || (existing.name.is_none() && path.is_none()))
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination '{}' is already registered under another name or backend; use its registered name or select it with --path",
            target.display()
        )));
    }
    let target_exists = match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(WorktreeError::InvalidTarget(format!(
                "worktree destination '{}' must be a real directory, not a symlink or file",
                target.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(WorktreeError::IoRead(format!(
                "cannot inspect worktree destination '{}': {error}",
                target.display()
            )));
        }
    };
    let canonical_target = target.clone();
    let _repository_ref_lease = acquire_worktree_ref_lease().await?;
    let mut state = load_state()?;
    if let Some(existing_index) = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == canonical_target)
    {
        if state.entries[existing_index].is_main {
            return Err(WorktreeError::MainWorktree {
                action: "name",
                path: target.to_string_lossy().into_owned(),
            });
        }
        let adopting_name = state.entries[existing_index].name.is_none();
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
                if adopting_name {
                    RegisteredDirectoryGuard::open(
                        &canonical_target,
                        state.entries[existing_index].worktree_id.as_deref(),
                    )?;
                    // Deliberate NAME + --path adoption only. Reattachment's
                    // identity/common checks run before its registry write.
                    state.entries[existing_index].name = Some(name.clone());
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
                if !target_exists && adopting_name {
                    return Err(WorktreeError::OperationBlocked(format!(
                        "worktree '{name}' is registered at '{}', but its directory is missing; restore it or run worktree prune before reusing the name",
                        target.display()
                    )));
                }
                if target_spec.is_some() || detach || new_branch.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is already a registered worktree; switch branches inside \
                          it instead",
                        canonical_target.display()
                    )));
                }
                if adopting_name {
                    let guard = RegisteredDirectoryGuard::open(
                        &canonical_target,
                        state.entries[existing_index].worktree_id.as_deref(),
                    )?;
                    guard.verify()?;
                    state.entries[existing_index].name = Some(name.clone());
                    write_state(&state)?;
                }
                return Ok(WorktreeAddOutput {
                    path: canonical_target.to_string_lossy().to_string(),
                    already_exists: true,
                    reattached: false,
                });
            }
        }
    }

    if !force
        && target_exists
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
            "target directory exists and is not empty: {} (use --force to reuse it)",
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
                  name or check the existing branch out with `worktree add <name> --path <path> {name}`"
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

    let created_target = !target_exists;
    let target_directory = if created_target {
        if target.starts_with(worktree_layout::managed_root(&storage)) {
            worktree_layout::create_managed_directory(&storage, &target)
        } else {
            let parent = target.parent().ok_or_else(|| {
                WorktreeError::InvalidTarget("worktree destination has no parent directory".into())
            })?;
            fs::create_dir_all(parent)
                .and_then(|_| fs::create_dir(&target))
                .and_then(|_| crate::utils::beneath::open_root(&target))
        }
    } else if target.starts_with(worktree_layout::managed_root(&storage)) {
        worktree_layout::open_managed_directory(&storage, &target)
    } else {
        crate::utils::beneath::open_root(&target)
    }
    .map_err(|source| {
        WorktreeError::IoWrite(format!(
            "cannot create or pin worktree directory '{}': {source}",
            target.display()
        ))
    })?;
    let target_identity =
        crate::utils::beneath::entry_identity_beneath(&target_directory, Path::new("")).map_err(
            |source| {
                WorktreeError::IoRead(format!(
                    "cannot identify new worktree '{}': {source}",
                    target.display()
                ))
            },
        )?;
    let mut target_entries =
        crate::utils::beneath::read_dir_fd(target_directory.try_clone().map_err(|error| {
            WorktreeError::IoRead(format!("cannot inspect pinned worktree directory: {error}"))
        })?)
        .map_err(|error| {
            WorktreeError::IoRead(format!("cannot inspect pinned worktree directory: {error}"))
        })?;
    // `--force` (Git's `worktree add -f`) lets an existing non-empty
    // destination through; a `.libra` entry inside it is still refused below.
    if !force
        && target_entries
            .next()
            .transpose()
            .map_err(|error| {
                WorktreeError::IoRead(format!("cannot inspect pinned worktree directory: {error}"))
            })?
            .is_some()
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "target directory is no longer empty: {} (use --force to reuse it)",
            target.display()
        )));
    }
    drop(target_entries);
    let link_path = target.join(util::ROOT_DIR);
    match crate::utils::beneath::lstat_beneath(&target_directory, Path::new(util::ROOT_DIR)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(WorktreeError::OperationBlocked(format!(
                "target already contains a .libra entry: {}",
                link_path.display()
            )));
        }
        Err(error) => {
            return Err(WorktreeError::IoRead(format!(
                "cannot inspect worktree metadata '{}': {error}",
                link_path.display()
            )));
        }
    }

    let worktree_id = util::worktree_instance_id(&canonical_target);
    // W1 §C.4.1.1: instance ids are DETERMINISTIC (path-derived), so a
    // worktree re-added where one was previously removed would inherit any
    // scoped rows a best-effort remove/prune GC failed to delete — stale
    // sparse filters would silently re-gate ls-files/diff/hydrate, stale
    // layer ownership would block staging. Sweep the scope STRICTLY before
    // seeding; a sweep failure fails the add (fail closed, nothing seeded).
    let db = WorktreeDb::open_current_read_write()
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    gc_worktree_scoped_rows_strict(&db, &worktree_id, true, &state, &canonical_target)
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
    let mut target_guard = RegisteredDirectoryGuard {
        target: target.clone(),
        common: storage.clone(),
        directory: Some(target_directory),
        identity: target_identity,
        worktree_id: worktree_id.clone(),
    };
    let rollback_partial_add = |guard: &mut RegisteredDirectoryGuard| {
        if let Err(error) = clear_pinned_worktree(guard, created_target) {
            tracing::warn!(path = %target.display(), %error, "partial worktree cleanup refused; the original directory may have moved");
        }
    };
    let metadata_created = target_guard
        .directory()
        .and_then(|directory| create_worktree_gitdir(&storage, directory, &worktree_id))
        .map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create per-worktree .libra gitdir in '{}': {source}",
                link_path.display()
            ))
        })
        .and_then(|()| target_guard.verify());
    if let Err(error) = metadata_created {
        rollback_partial_add(&mut target_guard);
        let _ = journal_resolve(&db, add_journal_id).await;
        return Err(error);
    }

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
                rollback_partial_add(&mut target_guard);
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
                        rollback_partial_add(&mut target_guard);
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "cannot verify whether branch '{name}' is checked out: {error}"
                        )));
                    }
                    Ok(Some(scope)) => {
                        rollback_partial_add(&mut target_guard);
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
                        rollback_partial_add(&mut target_guard);
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::InvalidTarget(format!(
                            "branch '{name}' disappeared while creating the worktree"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add(&mut target_guard);
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
                        rollback_partial_add(&mut target_guard);
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' was created concurrently; pick another name"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add(&mut target_guard);
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
                    rollback_partial_add(&mut target_guard);
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
                rollback_partial_add(&mut target_guard);
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
            rollback_partial_add(&mut target_guard);
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
            rollback_partial_add(&mut target_guard);
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

    if let Err(error) = target_guard.verify() {
        rollback_partial_add(&mut target_guard);
        rollback_created_branch(created_branch).await;
        return Err(error);
    }
    let registration_epoch = state.next_epoch();
    // A linked worktree exists from here on, and that fact must OUTLIVE its
    // entry: the ambiguous-sidecar rules ask "did one ever exist", and a later
    // removal deletes the entry (§C.4.3).
    state.linked_history = LinkedHistory::Existed;
    state.entries.push(WorktreeEntry {
        name: Some(name),
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
        rollback_partial_add(&mut target_guard);
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
    let db = WorktreeDb::open_current_read_write()
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let Some(expected_id) = state.entries[index].worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': the registry entry has no persisted worktree id; run \
              `libra worktree repair --confirm` first",
            target.display()
        )));
    };
    let directory_guard = RegisteredDirectoryGuard::open(target, Some(&expected_id))?;

    // Refuse a dormant duplicate before journalling an Add intent. The caller
    // holds the registry lock until activation, so no other entry can claim
    // this identity between the check and the registry write.
    if state.entries.iter().enumerate().any(|(other, entry)| {
        other != index
            && !entry.is_main
            && entry.state.is_active()
            && entry.worktree_id.as_deref() == Some(expected_id.as_str())
    }) {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': identity '{expected_id}' is already claimed by another ACTIVE \
              worktree. Back up both directories and keep the detached entry for manual recovery",
            state.entries[index].path
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
    directory_guard.verify()?;
    state.entries[index].state = WorktreeEntryState::Active;
    write_state(state)?;
    directory_guard.remove_detached_marker()?;
    // The shared mirror also describes any other detached claimant. Keep it
    // until that registration is resolved, even though this one is active.
    if !state.entries.iter().enumerate().any(|(other, entry)| {
        other != index
            && !entry.is_main
            && entry.worktree_id.as_deref() == Some(expected_id.as_str())
    }) && let Err(error) = lifecycle_delete(&db, &expected_id).await
    {
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

pub(crate) enum WorktreeDb {
    Sqlite(sea_orm::DatabaseConnection),
    Redb {
        git: std::sync::Arc<dyn crate::storage::GitStore>,
        state: std::sync::Arc<dyn crate::storage::GitStateStore>,
        config: std::sync::Arc<dyn crate::storage::ConfigStore>,
    },
}

pub(crate) fn workspace_record_from_row(
    row: crate::storage::WorkspaceRecordRow,
) -> crate::internal::workspace::WorkspaceRecord {
    use crate::{
        internal::workspace::{WorkspaceKind, WorkspaceOwnerKind, WorkspaceRecord, WorkspaceState},
        storage::{WorkspaceRecordKind, WorkspaceRecordOwnerKind, WorkspaceRecordState},
    };
    let kind = match row.kind {
        WorkspaceRecordKind::Linked => WorkspaceKind::Linked,
        WorkspaceRecordKind::TaskCopy => WorkspaceKind::TaskCopy,
        WorkspaceRecordKind::TaskFuse => WorkspaceKind::TaskFuse,
        WorkspaceRecordKind::Remote => WorkspaceKind::Remote,
    };
    let owner_kind = match row.owner_kind {
        WorkspaceRecordOwnerKind::Human => WorkspaceOwnerKind::Human,
        WorkspaceRecordOwnerKind::Agent => WorkspaceOwnerKind::Agent,
        WorkspaceRecordOwnerKind::Automation => WorkspaceOwnerKind::Automation,
    };
    let state = match row.state {
        WorkspaceRecordState::Provisioning => WorkspaceState::Provisioning,
        WorkspaceRecordState::Active => WorkspaceState::Active,
        WorkspaceRecordState::Releasing => WorkspaceState::Releasing,
        WorkspaceRecordState::Released => WorkspaceState::Released,
        WorkspaceRecordState::Orphaned => WorkspaceState::Orphaned,
    };
    WorkspaceRecord {
        workspace_id: row.workspace_id,
        repo_id: row.repo_id,
        kind,
        worktree_id: row.worktree_id,
        path: row.path,
        owner_kind,
        owner_id: row.owner_id,
        task_id: row.task_id,
        session_id: row.session_id,
        base_commit: row.base_commit,
        branch: row.branch,
        state,
        lease_owner: row.lease_owner,
        lease_fence: row.lease_fence,
        lease_expires_at: row.lease_expires_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

impl WorktreeDb {
    pub(crate) async fn open_current_read_write() -> Result<Self, String> {
        match crate::storage::runtime::detect_current_repo_backend() {
            Ok(crate::storage::StorageBackendKind::Redb) => {
                let git = crate::storage::runtime::open_current_git_store_read_write()
                    .await
                    .map_err(|e| format!("cannot open redb git store: {e}"))?;
                let state = crate::storage::runtime::open_current_git_state_store_read_write()
                    .await
                    .map_err(|e| format!("cannot open redb state store: {e}"))?;
                let config = crate::storage::runtime::open_current_repo_config_store_read_write()
                    .await
                    .map_err(|e| format!("cannot open redb config store: {e}"))?;
                Ok(Self::Redb { git, state, config })
            }
            Ok(crate::storage::StorageBackendKind::Sqlite) | Err(_) => Ok(Self::Sqlite(
                crate::internal::db::get_db_conn_instance().await,
            )),
        }
    }

    pub(crate) async fn open_current_read_only() -> Result<Self, String> {
        match crate::storage::runtime::detect_current_repo_backend() {
            Ok(crate::storage::StorageBackendKind::Redb) => {
                let git = crate::storage::runtime::open_current_git_store_read_only()
                    .await
                    .map_err(|e| format!("cannot open redb git store: {e}"))?;
                let state = crate::storage::runtime::open_current_git_state_store_read_only()
                    .await
                    .map_err(|e| format!("cannot open redb state store: {e}"))?;
                let config = crate::storage::runtime::open_current_repo_config_store_read_only()
                    .await
                    .map_err(|e| format!("cannot open redb config store: {e}"))?;
                Ok(Self::Redb { git, state, config })
            }
            Ok(crate::storage::StorageBackendKind::Sqlite) | Err(_) => {
                let db_path = crate::utils::path::database();
                let conn = crate::internal::db::open_database_without_migrations(&db_path)
                    .await
                    .map_err(|error| {
                        format!(
                            "cannot open the repository database at '{}': {error}",
                            db_path.display()
                        )
                    })?;
                Ok(Self::Sqlite(conn))
            }
        }
    }

    pub(crate) async fn find_live_linked(
        &self,
        worktree_id: &str,
    ) -> Result<
        Option<crate::internal::workspace::WorkspaceRecord>,
        crate::internal::workspace::WorkspaceError,
    > {
        match self {
            Self::Redb { state, config, .. } => {
                let Some(repo_id) = config
                    .get("libra.repoid")
                    .await
                    .map_err(|e| {
                        crate::internal::workspace::WorkspaceError::ReadFailed(e.to_string())
                    })?
                    .map(|e| e.value)
                    .filter(|v| !v.trim().is_empty())
                else {
                    return Ok(None);
                };
                let records = crate::internal::workspace::WorkspaceStore::list_with_store(
                    state.as_ref(),
                    &repo_id,
                    None,
                )
                .await
                .map_err(|e| {
                    crate::internal::workspace::WorkspaceError::ReadFailed(e.to_string())
                })?;
                Ok(records
                    .into_iter()
                    .map(workspace_record_from_row)
                    .find(|r| {
                        r.state.holds_identity() && r.worktree_id.as_deref() == Some(worktree_id)
                    }))
            }
            Self::Sqlite(conn) => {
                crate::internal::workspace::WorkspaceStore::find_live_linked_with_conn(
                    conn,
                    worktree_id,
                )
                .await
            }
        }
    }

    pub(crate) async fn find_live_by_path(
        &self,
        path: &Path,
    ) -> Result<
        Option<crate::internal::workspace::WorkspaceRecord>,
        crate::internal::workspace::WorkspaceError,
    > {
        match self {
            Self::Redb { state, config, .. } => {
                let Some(repo_id) = config
                    .get("libra.repoid")
                    .await
                    .map_err(|e| {
                        crate::internal::workspace::WorkspaceError::ReadFailed(e.to_string())
                    })?
                    .map(|e| e.value)
                    .filter(|v| !v.trim().is_empty())
                else {
                    return Ok(None);
                };
                let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                let records = crate::internal::workspace::WorkspaceStore::list_with_store(
                    state.as_ref(),
                    &repo_id,
                    None,
                )
                .await
                .map_err(|e| {
                    crate::internal::workspace::WorkspaceError::ReadFailed(e.to_string())
                })?;
                Ok(records
                    .into_iter()
                    .map(workspace_record_from_row)
                    .find(|r| {
                        if !r.state.holds_identity() {
                            return false;
                        }
                        let rp = Path::new(&r.path);
                        rp == path
                            || rp == canon
                            || fs::canonicalize(rp).ok().as_deref() == Some(canon.as_path())
                    }))
            }
            Self::Sqlite(conn) => {
                crate::internal::workspace::WorkspaceStore::find_live_by_path_with_conn(conn, path)
                    .await
            }
        }
    }
}

pub(crate) async fn lifecycle_upsert(
    db: &WorktreeDb,
    worktree_id: &str,
    state: &str,
    path: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    match db {
        WorktreeDb::Redb { state: store, .. } => {
            let parsed_state = crate::storage::WorktreeLifecycleState::parse(state)
                .map_err(|e| format!("invalid worktree lifecycle state '{state}': {e}"))?;
            let rec = crate::storage::WorktreeLifecycleRecord {
                worktree_id: worktree_id.to_string(),
                state: parsed_state,
                path: path.to_string(),
                reason: None,
                created_at: now,
                updated_at: now,
            };
            store
                .upsert_worktree_lifecycle(&rec)
                .await
                .map_err(|error| format!("cannot record worktree lifecycle state: {error}"))
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            conn.execute_raw(Statement::from_sql_and_values(
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
    }
}

pub(crate) async fn lifecycle_delete(db: &WorktreeDb, worktree_id: &str) -> Result<(), String> {
    match db {
        WorktreeDb::Redb { state: store, .. } => store
            .delete_worktree_lifecycle(worktree_id)
            .await
            .map(|_| ())
            .map_err(|error| format!("cannot clear worktree lifecycle state: {error}")),
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "DELETE FROM worktree_lifecycle WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|error| format!("cannot clear worktree lifecycle state: {error}"))?;
            Ok(())
        }
    }
}

pub(crate) async fn lifecycle_rows(db: &WorktreeDb) -> Result<Vec<(String, String)>, String> {
    match db {
        WorktreeDb::Redb { state: store, .. } => {
            let rows = store
                .list_worktree_lifecycles()
                .await
                .map_err(|error| format!("cannot read the worktree lifecycle mirror: {error}"))?;
            Ok(rows
                .into_iter()
                .map(|r| (r.worktree_id, r.state.as_str().to_string()))
                .collect())
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            let rows = conn
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
    }
}

pub(crate) fn probe_path(path: &Path) -> PathPresence {
    match fs::symlink_metadata(path) {
        Ok(_) => PathPresence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathPresence::Missing,
        Err(error) => PathPresence::Unknown(error.to_string()),
    }
}

pub(crate) async fn journal_append(
    db: &WorktreeDb,
    op: &str,
    worktree_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64, String> {
    let now = chrono::Utc::now().timestamp_millis();
    match db {
        WorktreeDb::Redb { state: store, .. } => {
            let parsed_op = crate::storage::WorktreeIntentOp::parse(op)
                .map_err(|e| format!("invalid worktree intent op '{op}': {e}"))?;
            let write = crate::storage::WorktreeIntentWrite {
                op: parsed_op,
                worktree_id: worktree_id.map(str::to_string),
                payload: payload.to_string(),
                stage: None,
                created_at: now,
            };
            let rec = store
                .append_worktree_intent(&write)
                .await
                .map_err(|error| format!("cannot record the {op} intent journal entry: {error}"))?;
            Ok(rec.id)
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            let result = conn
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
    }
}

pub(crate) async fn journal_set_stage(db: &WorktreeDb, id: i64, stage: &str) -> Result<(), String> {
    match db {
        WorktreeDb::Redb { state: store, .. } => store
            .update_worktree_intent_stage(id, Some(stage))
            .await
            .map(|_| ())
            .map_err(|error| format!("cannot record migration stage {stage}: {error}")),
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE worktree_intent_journal SET stage = ? WHERE id = ?",
                [stage.into(), id.into()],
            ))
            .await
            .map_err(|error| format!("cannot record migration stage {stage}: {error}"))?;
            Ok(())
        }
    }
}

pub(crate) async fn journal_resolve(db: &WorktreeDb, id: i64) -> Result<(), String> {
    match db {
        WorktreeDb::Redb { state: store, .. } => store
            .resolve_worktree_intent(id)
            .await
            .map(|_| ())
            .map_err(|error| format!("cannot resolve intent journal entry {id}: {error}")),
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "DELETE FROM worktree_intent_journal WHERE id = ?",
                [id.into()],
            ))
            .await
            .map_err(|error| format!("cannot resolve intent journal entry {id}: {error}"))?;
            Ok(())
        }
    }
}

pub(crate) async fn journal_pending(db: &WorktreeDb) -> Result<Vec<PendingIntent>, String> {
    match db {
        WorktreeDb::Redb { state: store, .. } => {
            let rows = store
                .list_worktree_intents()
                .await
                .map_err(|error| format!("cannot read the intent journal: {error}"))?;
            let mut pending = Vec::with_capacity(rows.len());
            for row in rows {
                let payload = serde_json::from_str(&row.payload).map_err(|error| {
                    format!("corrupt intent journal payload (id {}): {error}", row.id)
                })?;
                pending.push(PendingIntent {
                    id: row.id,
                    op: row.op.as_str().to_string(),
                    worktree_id: row.worktree_id,
                    payload,
                });
            }
            Ok(pending)
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            let rows = conn
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
                let worktree_id: Option<String> = row.try_get_by_index(2).map_err(|error| {
                    format!("corrupt intent journal row (worktree_id): {error}")
                })?;
                let payload_raw: String = row
                    .try_get_by_index(3)
                    .map_err(|error| format!("corrupt intent journal row (payload): {error}"))?;
                let payload = serde_json::from_str(&payload_raw).map_err(|error| {
                    format!("corrupt intent journal payload (id {id}): {error}")
                })?;
                pending.push(PendingIntent {
                    id,
                    op,
                    worktree_id,
                    payload,
                });
            }
            Ok(pending)
        }
    }
}

pub(crate) async fn scoped_state_active(db: &WorktreeDb, worktree_id: &str) -> bool {
    match db {
        WorktreeDb::Redb { state: store, .. } => {
            match store.get_sequence_state(worktree_id).await {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => {}
            }
            match store.get_rebase_state(worktree_id).await {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => {}
            }
            match store.get_bisect_state(worktree_id).await {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => {}
            }
            false
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            for table in ["sequence_state", "rebase_state", "bisect_state"] {
                let query = format!("SELECT COUNT(*) FROM {table} WHERE worktree_id = ?");
                match conn
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
    }
}

pub(crate) async fn gc_worktree_scoped_rows_strict(
    db: &WorktreeDb,
    worktree_id: &str,
    directory_gone: bool,
    state: &WorktreeState,
    target: &Path,
) -> Result<(), String> {
    // A legacy identity collision can leave two entries with the same scope,
    // even after both have been detached. Never GC another registered entry's
    // rows, including from repair of an old pending journal or tombstone.
    if let Some(other) = state.entries.iter().find(|entry| {
        !entry.is_main
            && entry.worktree_id.as_deref() == Some(worktree_id)
            && Path::new(&entry.path) != target
    }) {
        return Err(format!(
            "identity '{worktree_id}' is still shared with registered worktree '{}'; refusing scoped cleanup",
            other.path
        ));
    }
    match db {
        WorktreeDb::Redb {
            git, state: store, ..
        } => {
            let head_key = crate::storage::RefKey::LocalHead {
                worktree_id: Some(worktree_id.to_string()),
            };
            if let Some(existing) = git
                .get_ref(&head_key)
                .await
                .map_err(|e| format!("inspect worktree HEAD: {e}"))?
            {
                let _ = git
                    .update_ref_cas(&head_key, Some(&existing), None, &[])
                    .await
                    .map_err(|e| format!("delete worktree HEAD: {e}"))?;
            }
            let reflogs = git
                .list_reflog("HEAD", Some(worktree_id), None)
                .await
                .map_err(|e| format!("list worktree reflog: {e}"))?;
            let ids: Vec<i64> = reflogs.into_iter().map(|r| r.id).collect();
            if !ids.is_empty() {
                let _ = git
                    .delete_reflog_entries("HEAD", Some(worktree_id), &ids)
                    .await
                    .map_err(|e| format!("delete worktree reflog: {e}"))?;
            }
            let _ = store
                .delete_sequence_state(worktree_id)
                .await
                .map_err(|e| format!("delete worktree sequence_state: {e}"))?;
            let _ = store
                .delete_rebase_state(worktree_id)
                .await
                .map_err(|e| format!("delete worktree rebase_state: {e}"))?;
            let _ = store
                .delete_bisect_state(worktree_id)
                .await
                .map_err(|e| format!("delete worktree bisect_state: {e}"))?;
            let empty_dirty = crate::storage::WorkingDirtyMetaRecord {
                worktree_id: worktree_id.to_string(),
                state: "clean".to_string(),
                index_fingerprint: None,
                head_oid: None,
                scanned_at: None,
                scan_lock_pid: None,
                scan_lock_at: None,
            };
            store
                .replace_working_dirty_snapshot(&empty_dirty, &[])
                .await
                .map_err(|e| format!("clear worktree working_dirty: {e}"))?;
            if directory_gone {
                let layers = store
                    .list_layers(worktree_id)
                    .await
                    .map_err(|e| format!("list worktree layers: {e}"))?;
                for layer in layers {
                    let _ = store
                        .delete_layer(worktree_id, &layer.name)
                        .await
                        .map_err(|e| format!("delete worktree layer: {e}"))?;
                }
                store
                    .replace_layer_paths(worktree_id, &[])
                    .await
                    .map_err(|e| format!("clear worktree layer_paths: {e}"))?;
                store
                    .clear_sparse_view(worktree_id)
                    .await
                    .map_err(|e| format!("clear worktree sparse_view: {e}"))?;
            }
            Ok(())
        }
        WorktreeDb::Sqlite(conn) => {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            let mut stmts = vec![
                "DELETE FROM reference WHERE worktree_id = ? AND kind = 'Head'",
                "DELETE FROM reflog WHERE worktree_id = ?",
                "DELETE FROM sequence_state WHERE worktree_id = ?",
                "DELETE FROM rebase_state WHERE worktree_id = ?",
                "DELETE FROM working_dirty WHERE worktree_id = ?",
                "DELETE FROM working_dirty_meta WHERE worktree_id = ?",
            ];
            if directory_gone {
                stmts.push("DELETE FROM layer WHERE worktree_id = ?");
                stmts.push("DELETE FROM layer_path WHERE worktree_id = ?");
                stmts.push("DELETE FROM sparse_view WHERE worktree_id = ?");
                stmts.push("DELETE FROM sparse_view_meta WHERE worktree_id = ?");
            }
            let has_bisect_table = conn
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
                conn.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    sql,
                    [worktree_id.into()],
                ))
                .await
                .map_err(|e| format!("{sql}: {e}"))?;
            }
            Ok(())
        }
    }
}

pub(crate) fn create_worktree_gitdir(
    common_storage: &Path,
    worktree_directory: &fs::File,
    worktree_id: &str,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let name = c".libra";
        // SAFETY: mkdirat creates one exclusive child of the held worktree;
        // an existing directory or symlink is an error, never adopted.
        if unsafe { libc::mkdirat(worktree_directory.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    {
        crate::utils::windows_fs::create_directory_at(
            worktree_directory,
            std::ffi::OsStr::new(".libra"),
            false,
        )?;
    }
    let gitdir = crate::utils::beneath::open_beneath(worktree_directory, Path::new(".libra"))?;
    crate::utils::beneath::write_regular_file_beneath(
        &gitdir,
        Path::new("commondir"),
        format!("{}\n", common_storage.display()).as_bytes(),
        true,
    )?;
    crate::utils::beneath::write_regular_file_beneath(
        &gitdir,
        Path::new("worktree_id"),
        format!("{worktree_id}\n").as_bytes(),
        true,
    )
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
                name: w.name,
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
        if let Some(name) = &w.name {
            out.push_str(&format!("name {name}\n"));
        }
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

/// Caller holds the common registry lock; path selectors are already resolved.
pub(crate) async fn lock_worktree_locked(
    path: String,
    reason: Option<String>,
) -> WorktreeResult<WorktreeLockOutput> {
    let mut state = load_state()?;
    let target = PathBuf::from(&path);
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

/// Caller holds the common registry lock; path selectors are already resolved.
pub(crate) async fn unlock_worktree_locked(path: String) -> WorktreeResult<WorktreeUnlockOutput> {
    let mut state = load_state()?;
    let target = PathBuf::from(&path);
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

/// Caller holds the common registry lock; path selectors are already resolved.
pub(crate) async fn move_worktree_locked(
    src: String,
    dest: String,
) -> WorktreeResult<WorktreeMoveOutput> {
    let mut state = load_state()?;
    let src_path = PathBuf::from(&src);
    let dest_path = resolve_path(&dest, "destination worktree path")?;
    let storage = util::storage_path();

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

    if let Some(name) = state.entries[index].name.as_deref() {
        crate::utils::worktree_layout::validate_linked_destination(&storage, name, &dest_path)
            .map_err(|error| {
                WorktreeError::InvalidTarget(format!(
                    "invalid worktree destination '{}': {error}",
                    dest_path.display()
                ))
            })?;
    } else if util::is_sub_path(&dest_path, &storage) || util::is_sub_path(&storage, &dest_path) {
        return Err(WorktreeError::InvalidTarget(
            "unnamed worktrees cannot be moved into repository metadata; select an external --path"
                .into(),
        ));
    }
    if registered_worktree_locations()?
        .iter()
        .any(|entry| entry.path == dest_path)
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination is already registered: {}",
            dest_path.display()
        )));
    }
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

    if detect_entry_layout(&src_path, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "legacy worktree '{}' uses shared .libra metadata; run `libra worktree repair --confirm --migrate-layout --path {}` before moving it",
            src_path.display(),
            src_path.display()
        )));
    }
    let mut directory_guard =
        RegisteredDirectoryGuard::open(&src_path, state.entries[index].worktree_id.as_deref())?;
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
        let db = WorktreeDb::open_current_read_write()
            .await
            .map_err(WorktreeError::OperationBlocked)?;
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
    if dest_path.starts_with(crate::utils::worktree_layout::managed_root(&storage))
        && let Some(parent) = dest_path.parent()
    {
        crate::utils::worktree_layout::ensure_managed_directory(&storage, parent).map_err(
            |error| {
                WorktreeError::IoWrite(format!(
                    "cannot create worktree destination parent '{}': {error}",
                    parent.display()
                ))
            },
        )?;
    }
    let payload = serde_json::json!({
        "src": src_path.to_string_lossy(),
        "dest": dest_path.to_string_lossy(),
    });
    let db = WorktreeDb::open_current_read_write()
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let journal_id = journal_append(
        &db,
        WorktreeControl::Move.declare(),
        journal_worktree_id.as_deref(),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    directory_guard.verify()?;
    let old_path = state.entries[index].path.clone();
    state.entries[index].path = dest_path.to_string_lossy().to_string();
    if let Err(e) = write_state(&state) {
        state.entries[index].path = old_path;
        let _ = journal_resolve(&db, journal_id).await;
        return Err(e);
    }

    // A cross-filesystem copy cannot preserve symlinks and unrelated sibling
    // directories safely through the old fs_extra fallback. Keep the source
    // authoritative when an atomic rename is unavailable.
    let move_result = rename_pinned_worktree(&mut directory_guard, &dest_path);
    if let Err(e) = move_result {
        let source_unchanged = crate::utils::beneath::open_root(&src_path)
            .and_then(|directory| {
                crate::utils::beneath::entry_identity_beneath(&directory, Path::new(""))
            })
            .is_ok_and(|identity| identity == directory_guard.identity);
        // A failed rename never copied data. Restore the original registration
        // only while the pinned source still has its original identity.
        if source_unchanged {
            state.entries[index].path = old_path;
            write_state(&state)?;
            let _ = journal_resolve(&db, journal_id).await;
            if matches!(
                e.kind(),
                io::ErrorKind::CrossesDevices | io::ErrorKind::Unsupported
            ) {
                return Err(WorktreeError::OperationBlocked(format!(
                    "cannot atomically move worktree '{}' to '{}' across filesystems; the source and registration were kept. Choose a destination on the same filesystem, or create a separate worktree at the destination and transfer changes explicitly",
                    src_path.display(),
                    dest_path.display()
                )));
            }
        }
        return Err(WorktreeError::IoWrite(format!(
            "failed to move worktree directory '{}' to '{}': {e}; inspect both paths and run `libra worktree repair --confirm` if the source directory changed",
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
    prune_worktrees_locked().await
}

/// Caller holds the common registry lock; path selectors are already resolved.
pub(crate) async fn prune_worktrees_locked() -> WorktreeResult<WorktreePruneOutput> {
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
        let db = WorktreeDb::open_current_read_write()
            .await
            .map_err(WorktreeError::OperationBlocked)?;
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
                match db.find_live_linked(id_str).await {
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
                    match gc_worktree_scoped_rows_strict(&db, id_str, true, &state, Path::new(path))
                        .await
                    {
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

/// Pins the directory identity while a lifecycle action awaits database or
/// dirty-state checks. Never treat a failed metadata read as a missing tree.
struct RegisteredDirectoryGuard {
    target: PathBuf,
    common: PathBuf,
    directory: Option<fs::File>,
    identity: crate::utils::beneath::EntryIdentity,
    worktree_id: String,
}

impl RegisteredDirectoryGuard {
    fn metadata_directory(&self) -> WorktreeResult<fs::File> {
        self.verify()?;
        self.directory()
            .and_then(|directory| {
                crate::utils::beneath::open_beneath(directory, Path::new(".libra"))
            })
            .and_then(|gitdir| {
                self.verify_metadata(&gitdir)?;
                Ok(gitdir)
            })
            .map_err(|error| {
                WorktreeError::IoRead(format!(
                    "cannot pin worktree metadata '{}': {error}",
                    self.target.display()
                ))
            })
    }

    fn write_detached_marker(&self) -> WorktreeResult<()> {
        use std::io::Write;
        let gitdir = self.metadata_directory()?;
        let name = Path::new(DETACHED_MARKER);
        let create = || -> io::Result<fs::File> {
            #[cfg(unix)]
            {
                use std::os::fd::{AsRawFd, FromRawFd};
                let name = std::ffi::CString::new(DETACHED_MARKER)
                    .map_err(|_| io::Error::other("invalid worktree marker name"))?;
                // SAFETY: the held gitdir and single component remain live;
                // exclusive no-follow creation never truncates an existing file.
                let fd = unsafe {
                    libc::openat(
                        gitdir.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        0o600,
                    )
                };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: openat returned an owned file descriptor.
                Ok(unsafe { fs::File::from_raw_fd(fd) })
            }
            #[cfg(windows)]
            {
                crate::utils::windows_fs::create_private_file_at(&gitdir, name.as_os_str())
            }
        };
        let result = match create() {
            Ok(mut file) => file.write_all(format!("{}\nremoved from the worktree registry; re-add or delete this directory\n", self.worktree_id).as_bytes())
                .and_then(|()| file.sync_all()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                crate::utils::worktree_layout::read_regular_metadata_from(&gitdir, name, 4096)
                    .and_then(|bytes| {
                        if std::str::from_utf8(&bytes).ok().and_then(|text| text.lines().next()) == Some(self.worktree_id.as_str()) {
                            Ok(())
                        } else { Err(io::Error::other("existing detached marker has another identity; run worktree repair --confirm")) }
                    })
            }
            Err(error) => Err(error),
        };
        #[cfg(unix)]
        let result = result.and_then(|()| gitdir.sync_all());
        result.map_err(|error| {
            WorktreeError::IoWrite(format!(
                "cannot write detached marker for '{}': {error}",
                self.target.display()
            ))
        })
    }

    fn remove_detached_marker(&self) -> WorktreeResult<bool> {
        use crate::utils::beneath;
        let gitdir = self.metadata_directory()?;
        let name = Path::new(DETACHED_MARKER);
        let result = (|| -> io::Result<bool> {
            let identity = match beneath::entry_identity_beneath(&gitdir, name) {
                Ok(identity) => identity,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            if identity.kind != beneath::EntryKind::File {
                return Err(io::Error::other("detached marker is not a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                let name = std::ffi::CString::new(DETACHED_MARKER)
                    .map_err(|_| io::Error::other("invalid worktree marker name"))?;
                // SAFETY: unlink only the marker beneath the held gitdir.
                if unsafe { libc::unlinkat(gitdir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            #[cfg(windows)]
            {
                let entry = crate::utils::windows_fs::inspect_entry_at(&gitdir, name.as_os_str())?;
                crate::utils::windows_fs::remove_entry_at(
                    &gitdir,
                    name.as_os_str(),
                    entry.identity,
                    false,
                )?;
            }
            #[cfg(unix)]
            gitdir.sync_all()?;
            Ok(true)
        })();
        result.map_err(|error| {
            WorktreeError::IoWrite(format!(
                "cannot remove detached marker for '{}': {error}; run worktree repair --confirm",
                self.target.display()
            ))
        })
    }

    fn directory(&self) -> io::Result<&fs::File> {
        self.directory.as_ref().ok_or_else(|| {
            io::Error::other("the directory identity pin was released for final removal")
        })
    }

    fn open(target: &Path, worktree_id: Option<&str>) -> WorktreeResult<Self> {
        use crate::utils::{beneath, worktree_layout};
        let worktree_id = worktree_id.ok_or_else(|| WorktreeError::OperationBlocked(format!(
            "cannot change worktree '{}': its persisted identity is missing; run worktree repair --confirm first", target.display()
        )))?;
        let metadata = fs::symlink_metadata(target).map_err(|error| {
            WorktreeError::IoRead(format!("cannot inspect registered worktree '{}': {error}; restore it or use worktree prune if it is gone", target.display()))
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::OperationBlocked(format!(
                "registered worktree '{}' was replaced by a symlink or non-directory; refusing to change it",
                target.display()
            )));
        }
        let common = worktree_layout::current_common_storage().map_err(|error| {
            WorktreeError::IoRead(format!("cannot resolve common worktree storage: {error}"))
        })?;
        let directory = if target.starts_with(worktree_layout::managed_root(&common)) {
            worktree_layout::open_managed_directory(&common, target)
        } else {
            beneath::open_root(target)
        }
        .map_err(|error| {
            WorktreeError::IoRead(format!(
                "cannot pin registered worktree '{}': {error}",
                target.display()
            ))
        })?;
        let identity =
            beneath::entry_identity_beneath(&directory, Path::new("")).map_err(|error| {
                WorktreeError::IoRead(format!(
                    "cannot identify worktree '{}': {error}",
                    target.display()
                ))
            })?;
        let guard = Self {
            target: target.to_path_buf(),
            common,
            directory: Some(directory),
            identity,
            worktree_id: worktree_id.into(),
        };
        guard.verify()?;
        Ok(guard)
    }

    fn verify_metadata(&self, gitdir: &fs::File) -> io::Result<()> {
        let id = crate::utils::worktree_layout::read_regular_metadata_from(
            gitdir,
            Path::new("worktree_id"),
            1024,
        )?;
        let id = std::str::from_utf8(&id)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if id.trim() != self.worktree_id {
            return Err(io::Error::other(
                "the directory no longer carries the registered worktree identity",
            ));
        }
        let common = crate::utils::worktree_layout::read_regular_metadata_from(
            gitdir,
            Path::new("commondir"),
            65_536,
        )?;
        let common = std::str::from_utf8(&common)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if common.trim().is_empty() {
            return Err(io::Error::other(
                "the worktree common-storage pointer is invalid",
            ));
        }
        let pointer = Path::new(common.trim());
        let pointer = if pointer.is_absolute() {
            pointer.to_path_buf()
        } else {
            self.target.join(".libra").join(pointer)
        };
        if fs::canonicalize(pointer)? != fs::canonicalize(&self.common)? {
            return Err(io::Error::other(
                "the worktree common-storage pointer belongs to another repository",
            ));
        }
        Ok(())
    }

    fn verify(&self) -> WorktreeResult<()> {
        use crate::utils::beneath;
        let current = beneath::open_root(&self.target).and_then(|directory| {
            let identity = beneath::entry_identity_beneath(&directory, Path::new(""))?;
            let pinned = beneath::entry_identity_beneath(self.directory()?, Path::new(""))?;
            if identity != self.identity || pinned != self.identity {
                return Err(io::Error::other(
                    "the directory identity changed during the operation",
                ));
            }
            let gitdir = beneath::open_beneath(&directory, Path::new(".libra"))?;
            self.verify_metadata(&gitdir)?;
            Ok(())
        });
        current.map_err(|error| WorktreeError::OperationBlocked(format!("refusing to change worktree '{}': {error}; restore its original directory or repair its metadata", self.target.display())))
    }
}

fn rename_pinned_worktree(
    guard: &mut RegisteredDirectoryGuard,
    destination: &Path,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

        use crate::utils::beneath;
        let source_parent = guard
            .target
            .parent()
            .ok_or_else(|| io::Error::other("worktree source has no parent"))?;
        let source_name = guard
            .target
            .file_name()
            .ok_or_else(|| io::Error::other("worktree source has no directory name"))?;
        let destination_parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("worktree destination has no parent"))?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| io::Error::other("worktree destination has no directory name"))?;
        let source_parent = beneath::open_root(source_parent)?;
        let destination_parent = beneath::open_root(destination_parent)?;
        if beneath::entry_identity_beneath(&source_parent, Path::new(source_name))?
            != guard.identity
        {
            return Err(io::Error::other("worktree source changed before move"));
        }
        let source_component = std::ffi::CString::new(source_name.as_bytes())
            .map_err(|_| io::Error::other("worktree source contains a NUL byte"))?;
        let destination_component = std::ffi::CString::new(destination_name.as_bytes())
            .map_err(|_| io::Error::other("worktree destination contains a NUL byte"))?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: both directory descriptors and single component strings are
        // live. RENAME_NOREPLACE atomically refuses any destination entry.
        let result = unsafe {
            libc::renameat2(
                source_parent.as_raw_fd(),
                source_component.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_component.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        // SAFETY: same held-parent contract; RENAME_EXCL never replaces a leaf.
        let result = unsafe {
            libc::renameatx_np(
                source_parent.as_raw_fd(),
                source_component.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_component.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        )))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform does not support exclusive worktree moves",
        ));
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        ))]
        {
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            if beneath::entry_identity_beneath(&destination_parent, Path::new(destination_name))?
                != guard.identity
            {
                return Err(io::Error::other("worktree destination changed during move"));
            }
            Ok(())
        }
    }
    #[cfg(windows)]
    {
        use crate::utils::{beneath, windows_fs};
        let source_parent = guard
            .target
            .parent()
            .ok_or_else(|| io::Error::other("worktree source has no parent"))?;
        let source_name = guard
            .target
            .file_name()
            .ok_or_else(|| io::Error::other("worktree source has no directory name"))?;
        let destination_parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("worktree destination has no parent"))?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| io::Error::other("worktree destination has no directory name"))?;
        let source_parent = beneath::open_root(source_parent)?;
        let destination_parent = beneath::open_root(destination_parent)?;
        let identity = windows_fs::identity(guard.directory()?)?;
        drop(guard.directory.take());
        windows_fs::rename_entry_at(
            &source_parent,
            source_name,
            identity,
            &destination_parent,
            destination_name,
        )
    }
}

/// Remove entries through pinned directory handles; a path swap can never make
/// cleanup recursively traverse a different directory or follow a symlink.
fn delete_pinned_worktree(guard: &mut RegisteredDirectoryGuard) -> io::Result<()> {
    clear_pinned_worktree(guard, true)
}

fn clear_pinned_worktree(
    guard: &mut RegisteredDirectoryGuard,
    remove_root: bool,
) -> io::Result<()> {
    use crate::utils::beneath;
    fn remove_entry(
        parent: &fs::File,
        name: &std::ffi::OsStr,
        expected: beneath::EntryIdentity,
    ) -> io::Result<()> {
        if beneath::entry_identity_beneath(parent, Path::new(name))? != expected {
            return Err(io::Error::other("worktree entry changed before removal"));
        }
        #[cfg(unix)]
        {
            use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "worktree entry contains a NUL byte",
                )
            })?;
            let flags = if expected.kind == beneath::EntryKind::Directory {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            // SAFETY: unlinkat removes only this entry beneath the pinned
            // parent. It cannot follow a replacement symlink during cleanup.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            let entry = crate::utils::windows_fs::inspect_entry_at(parent, name)?;
            if beneath::entry_identity_beneath(parent, Path::new(name))? != expected {
                return Err(io::Error::other("worktree entry changed before removal"));
            }
            crate::utils::windows_fs::remove_entry_at(parent, name, entry.identity, entry.directory)
        }
    }
    fn verify_root(guard: &RegisteredDirectoryGuard) -> io::Result<()> {
        let named = beneath::open_root(&guard.target)?;
        if beneath::entry_identity_beneath(&named, Path::new(""))? != guard.identity {
            return Err(io::Error::other(
                "registered worktree directory changed during cleanup",
            ));
        }
        Ok(())
    }
    fn remove_contents(
        directory: &fs::File,
        depth: usize,
        guard: &RegisteredDirectoryGuard,
    ) -> io::Result<()> {
        verify_root(guard)?;
        if depth > 256 {
            return Err(io::Error::other(
                "worktree exceeds the safe cleanup depth; remove its deepest directories and retry",
            ));
        }
        for entry in beneath::read_dir_fd(directory.try_clone()?)? {
            let entry = entry?;
            let relative = Path::new(&entry.name);
            let expected = beneath::entry_identity_beneath(directory, relative)?;
            if expected.kind == beneath::EntryKind::Directory {
                let child = beneath::open_beneath(directory, relative)?;
                if beneath::entry_identity_beneath(&child, Path::new(""))? != expected {
                    return Err(io::Error::other(
                        "worktree directory changed during removal",
                    ));
                }
                remove_contents(&child, depth + 1, guard)?;
            }
            verify_root(guard)?;
            remove_entry(directory, &entry.name, expected)?;
        }
        Ok(())
    }
    let parent_path = guard
        .target
        .parent()
        .ok_or_else(|| io::Error::other("worktree has no parent directory"))?;
    let name = guard
        .target
        .file_name()
        .ok_or_else(|| io::Error::other("worktree has no directory name"))?;
    let parent = beneath::open_root(parent_path)?;
    if beneath::entry_identity_beneath(&parent, Path::new(name))? != guard.identity {
        return Err(io::Error::other(
            "registered worktree directory was replaced before cleanup",
        ));
    }
    remove_contents(guard.directory()?, 0, guard)?;
    verify_root(guard)?;
    if remove_root {
        // Windows directory pins deny DELETE sharing. Release this last child
        // handle before the parent-relative identity-checked unlink.
        drop(guard.directory.take());
        remove_entry(&parent, name, guard.identity)
    } else {
        Ok(())
    }
}

/// Caller holds the common registry lock; path selectors are already resolved.
pub(crate) async fn remove_worktree_locked(
    path: String,
    delete_dir: bool,
    force: bool,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let _repository_ref_lease = acquire_worktree_ref_lease().await?;
    let mut state = load_state()?;
    let target = PathBuf::from(&path);

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
    // A detached entry can share its legacy identity with another registered
    // entry even if that entry is also detached. Deleting it would GC its
    // identity-keyed HEAD, reflog and other scoped rows.
    if delete_dir
        && let Some(identity) = entry.worktree_id.as_deref()
        && state.entries.iter().enumerate().any(|(other, candidate)| {
            other != index
                && !candidate.is_main
                && candidate.worktree_id.as_deref() == Some(identity)
        })
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot delete '{}': identity '{identity}' is still shared with another registered \
             worktree; deleting it would remove that worktree's scoped state. Back up both \
             directories and keep the detached entry for manual recovery; automatic identity \
             migration is not available",
            target.display()
        )));
    }
    // W3-s3: a LEGACY symlink target shares main's `.libra` — any lifecycle
    // marker written into it would land in MAIN storage and freeze the main
    // repository. Refuse until the layout migration settles it.
    if detect_entry_layout(&target, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' uses the legacy shared-.libra symlink layout; run `libra worktree \
              repair --migrate-layout --confirm --path {}` first",
            target.display(),
            target.display()
        )));
    }
    let directory_guard = RegisteredDirectoryGuard::open(&target, entry.worktree_id.as_deref())?;
    // §C.7: a worktree an AGENT holds a live workspace lease on is not the
    // human's to detach or delete out from under it — the lease's fenced
    // owner is mid-work there. Release or let the lease lapse first.
    {
        let db = WorktreeDb::open_current_read_write()
            .await
            .map_err(WorktreeError::OperationBlocked)?;
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
        let by_path = db.find_live_by_path(&target).await.map_err(lease_error)?;
        let by_id = match entry.worktree_id.as_deref() {
            Some(id) => db.find_live_linked(id).await.map_err(lease_error)?,
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
    let db = WorktreeDb::open_current_read_write()
        .await
        .map_err(WorktreeError::OperationBlocked)?;

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

    directory_guard.verify()?;
    if delete_dir {
        drop(directory_guard);
        remove_worktree_delete_dir(
            &db,
            &mut state,
            index,
            &target,
            worktree_id_for_gc,
            entry_state,
            force,
        )
        .await
    } else {
        remove_worktree_detach(
            &db,
            &mut state,
            index,
            &target,
            worktree_id_for_gc,
            directory_guard,
        )
        .await
    }
}

async fn remove_worktree_detach(
    db: &WorktreeDb,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
    directory_guard: RegisteredDirectoryGuard,
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
    directory_guard.verify()?;
    state.entries[index].state = WorktreeEntryState::DetachedFromRegistry;
    write_state(state)?;
    // Marker BEFORE resolving the journal: if the marker write fails, the
    // pending row makes `worktree repair` re-freeze the directory. The DB
    // handle was resolved before the marker exists, so the resolve below
    // cannot trip the gate even when cwd is inside the target.
    directory_guard.write_detached_marker()?;
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

#[allow(clippy::too_many_arguments)]
async fn remove_worktree_delete_dir(
    db: &WorktreeDb,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
    entry_state: WorktreeEntryState,
    force: bool,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let mut directory_guard = RegisteredDirectoryGuard::open(target, worktree_id.as_deref())?;
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
        directory_guard.remove_detached_marker()?
    } else {
        false
    };

    let dirty = if force {
        // `--force` skips the dirty-state gate (Git's `worktree remove -f`).
        Ok(false)
    } else {
        worktree_is_dirty(target).await
    };
    match dirty {
        Ok(false) => {}
        Ok(true) => {
            let restored = directory_guard.verify().is_ok()
                && (entry_state != WorktreeEntryState::DetachedFromRegistry
                    || directory_guard.write_detached_marker().is_ok());
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(WorktreeError::DirtyWorktree {
                path: target.to_string_lossy().to_string(),
            });
        }
        Err(error) => {
            let restored = directory_guard.verify().is_ok()
                && (entry_state != WorktreeEntryState::DetachedFromRegistry
                    || directory_guard.write_detached_marker().is_ok());
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(error);
        }
    }

    // A swapped directory must never receive our detached marker or have
    // its contents removed. Leave the journal for explicit recovery.
    directory_guard.verify()?;
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
    if let Err(e) = delete_pinned_worktree(&mut directory_guard) {
        // Re-freeze a detached entry before surfacing the error — the
        // journal row stays pending either way (no resolve on this path),
        // so repair re-establishes whatever this restore could not.
        if directory_guard.verify().is_ok()
            && entry_state == WorktreeEntryState::DetachedFromRegistry
        {
            let _ = directory_guard.write_detached_marker();
        }
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
        match gc_worktree_scoped_rows_strict(db, id, true, state, target).await {
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
    let planned_cleanup_root = if cleanup {
        Some(fuse_utils::fuse_task_worktree_cleanup_root(&mountpoint).ok_or_else(|| {
            WorktreeError::InvalidTarget(format!(
                "--cleanup only supports legacy Libra task FUSE paths ending in '/workspace', not managed named worktrees: {}",
                mountpoint.display()
            ))
        })?)
    } else {
        None
    };
    fuse_utils::force_unmount_path(&mountpoint).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "failed to unmount FUSE path {}: {source}",
            mountpoint.display()
        ))
    })?;

    let mut cleanup_root = None;
    let mut cleanup_root_removed = false;
    if let Some(root) = planned_cleanup_root {
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

#[cfg(test)]
mod named_delete_tests {
    use std::{fs, path::Path};

    use super::{
        RegisteredDirectoryGuard, clear_pinned_worktree, delete_pinned_worktree,
        rename_pinned_worktree,
    };
    use crate::utils::beneath;

    fn pin(path: &Path) -> RegisteredDirectoryGuard {
        fs::create_dir_all(path.join(".libra")).unwrap();
        fs::write(path.join(".libra/worktree_id"), "test-worktree\n").unwrap();
        fs::write(
            path.join(".libra/commondir"),
            format!("{}\n", path.join(".libra").display()),
        )
        .unwrap();
        let directory = beneath::open_root(path).unwrap();
        let identity = beneath::entry_identity_beneath(&directory, Path::new("")).unwrap();
        RegisteredDirectoryGuard {
            target: path.to_path_buf(),
            common: path.join(".libra"),
            directory: Some(directory),
            identity,
            worktree_id: "test-worktree".into(),
        }
    }

    #[test]
    fn rollback_cleans_partial_metadata_and_preserves_an_existing_empty_root() {
        let temp = tempfile::tempdir().unwrap();
        for owned_root in [false, true] {
            let target = temp
                .path()
                .join(if owned_root { "owned" } else { "existing" });
            let mut guard = pin(&target);
            fs::remove_file(target.join(".libra/worktree_id")).unwrap();
            assert!(guard.verify().is_err());
            clear_pinned_worktree(&mut guard, owned_root).unwrap();
            assert_eq!(target.exists(), !owned_root);
            if !owned_root {
                assert!(fs::read_dir(target).unwrap().next().is_none());
            }
        }
    }

    #[test]
    fn lifecycle_marker_uses_the_pinned_metadata_directory() {
        let temp = tempfile::tempdir().unwrap();
        let guard = pin(&temp.path().join("worktree"));
        guard.write_detached_marker().unwrap();
        guard.write_detached_marker().unwrap();
        assert!(guard.remove_detached_marker().unwrap());
        assert!(!guard.remove_detached_marker().unwrap());
    }

    #[test]
    fn move_never_overwrites_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let mut guard = pin(&source);
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("user-file"), "keep").unwrap();
        assert!(rename_pinned_worktree(&mut guard, &destination).is_err());
        assert!(source.join(".libra/worktree_id").is_file());
        assert_eq!(
            fs::read_to_string(destination.join("user-file")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn move_refuses_a_replacement_source_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let mut guard = pin(&source);
        fs::rename(&source, temp.path().join("original")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("user-file"), "keep").unwrap();
        assert!(rename_pinned_worktree(&mut guard, &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(
            fs::read_to_string(source.join("user-file")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn marker_creation_does_not_truncate_a_preexisting_hardlink() {
        let temp = tempfile::tempdir().unwrap();
        let guard = pin(&temp.path().join("worktree"));
        let outside = temp.path().join("outside");
        fs::write(&outside, "test-worktree\nkeep original data\n").unwrap();
        fs::hard_link(
            &outside,
            guard.target.join(".libra").join(super::DETACHED_MARKER),
        )
        .unwrap();
        guard.write_detached_marker().unwrap();
        assert_eq!(
            fs::read_to_string(outside).unwrap(),
            "test-worktree\nkeep original data\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_markers_refuse_a_replaced_metadata_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let guard = pin(&temp.path().join("worktree"));
        let original = temp.path().join("original-metadata");
        fs::rename(guard.target.join(".libra"), &original).unwrap();
        std::os::unix::fs::symlink(&original, guard.target.join(".libra")).unwrap();
        assert!(guard.write_detached_marker().is_err());
        assert!(guard.remove_detached_marker().is_err());
        assert!(!original.join(super::DETACHED_MARKER).exists());
    }

    #[cfg(unix)]
    #[test]
    fn identity_checks_refuse_fifo_markers() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::tempdir().unwrap();
        let guard = pin(&temp.path().join("worktree"));
        guard.verify().unwrap();
        let marker = guard.target.join(".libra/worktree_id");
        fs::remove_file(&marker).unwrap();
        let marker = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
        // SAFETY: the temporary path is a valid C string and names a new FIFO.
        assert_eq!(unsafe { libc::mkfifo(marker.as_ptr(), 0o600) }, 0);
        assert!(guard.verify().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn deletion_refuses_a_replacement_directory() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("worktree");
        let original = temp.path().join("original");
        let mut guard = pin(&target);
        fs::rename(&target, &original).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("user-file"), "keep").unwrap();
        assert!(guard.verify().is_err());
        assert!(delete_pinned_worktree(&mut guard).is_err());
        assert_eq!(
            fs::read_to_string(target.join("user-file")).unwrap(),
            "keep"
        );
        assert!(original.join(".libra/worktree_id").exists());
    }

    #[cfg(windows)]
    #[test]
    fn deletion_releases_the_pin_before_unlinking_the_owned_directory() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("worktree");
        let mut guard = pin(&target);
        assert!(fs::rename(&target, temp.path().join("replacement")).is_err());
        guard.verify().unwrap();
        delete_pinned_worktree(&mut guard).unwrap();
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn deletion_unlinks_content_symlinks_without_visiting_their_targets() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("worktree");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("user-file"), "keep").unwrap();
        let mut guard = pin(&target);
        fs::create_dir(target.join("nested")).unwrap();
        fs::write(target.join("nested/tracked"), "content").unwrap();
        std::os::unix::fs::symlink(&outside, target.join("outside-link")).unwrap();
        delete_pinned_worktree(&mut guard).unwrap();
        assert!(!target.exists());
        assert_eq!(
            fs::read_to_string(outside.join("user-file")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn deletion_refuses_a_replacement_root_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("worktree");
        let mut guard = pin(&target);
        fs::rename(&target, temp.path().join("original")).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("user-file"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        assert!(guard.verify().is_err());
        assert!(delete_pinned_worktree(&mut guard).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("user-file")).unwrap(),
            "keep"
        );
    }
}
