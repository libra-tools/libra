//! Merge worktree writes: file/symlink materialization, path-clearing and
//! untracked-conflict fences shared by the tree-arbitration drivers.
#![allow(unused_imports)]
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

// Preserve the existing command::merge type path for downstream callers.
#[allow(unused_imports)]
pub(crate) use autostash::StoppedMerge;
pub(crate) use autostash::{
    MergeAutostash, conclude_merge_after_commit, conclude_stopped_merge, snapshot_stopped_merge,
};
use autostash::{
    preflight_held_autostash, prepare_merge_autostash, resolve_pending_autostash,
    resolve_pending_autostash_with, store_pending_autostash, verify_autostash_ownership,
};
use clap::{Parser, ValueEnum};
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            blob::Blob,
            commit::Commit,
            signature::{Signature, SignatureType},
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::{Deserialize, Serialize};
pub(crate) use state::{
    MergeState, merge_in_progress, merge_state_for_pseudo_refs, merge_state_gc_oids,
};

use super::{
    get_target_commit, load_object, load_object_raw, rename_detect, reset,
    restore::{self, RestoreArgs},
    save_object, status, switch, *,
};
use crate::{
    command::{
        commit::{CleanupMode, cleanup_commit_message, parse_cleanup_mode},
        editor,
    },
    common_utils::{format_commit_msg, parse_commit_msg},
    info_println,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        merge_base,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        repo_hooks::{
            RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook_with_io,
        },
        tree_plumbing,
    },
    utils::{
        attributes::{self, AttributeState},
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, util, worktree,
    },
};

pub(crate) fn write_workdir_entry(
    workdir: &Path,
    relative: &Path,
    mode: TreeItemMode,
    content: &[u8],
) -> Result<(), String> {
    if mode == TreeItemMode::Link {
        return write_workdir_symlink(workdir, relative, content);
    }
    write_workdir_file_with_mode(
        workdir,
        relative,
        content,
        mode == TreeItemMode::BlobExecutable,
    )
}

pub(crate) fn write_workdir_symlink(
    workdir: &Path,
    relative: &Path,
    content: &[u8],
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        refuse_symlink_components(workdir, relative)?;
        let full = workdir.join(relative);
        if let Some(parent) = full.parent() {
            // Same ancestor rule as the file writer: an ignored file standing
            // where a directory must go is replaced (Codex R10).
            clear_ancestor_files(workdir, relative)?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        clear_write_target(&full)?;
        std::os::unix::fs::symlink(OsStr::from_bytes(content), &full).map_err(|error| {
            format!(
                "failed to create the symbolic link {}: {error}",
                full.display()
            )
        })
    }
    #[cfg(not(unix))]
    {
        write_workdir_file(workdir, relative, content)
    }
}

pub(crate) fn worktree_paths_to_write(
    merged_items: &HashMap<PathBuf, MergeTreeEntry>,
) -> Vec<PathBuf> {
    merged_items
        .iter()
        .filter(|(_, entry)| entry.mode != TreeItemMode::Commit)
        .map(|(path, _)| path.clone())
        .collect()
}

pub(crate) fn ensure_no_untracked_conflicts(
    current_index: &Index,
    paths: &[PathBuf],
    gitlink_paths: &[PathBuf],
) -> Result<(), PullMergeError> {
    let untracked_paths =
        worktree::untracked_workdir_paths(current_index).map_err(PullMergeError::IndexLoad)?;
    for untracked in &untracked_paths {
        for path in paths {
            if worktree::paths_conflict(untracked, path) {
                return Err(PullMergeError::UntrackedOverwrite {
                    path: untracked.display().to_string(),
                });
            }
        }
        // A gitlink is matched on the EXACT path only. Libra writes no content
        // inside a submodule, so untracked files UNDER it are the submodule's
        // own checkout and are not overwritten — but a plain file or symlink
        // sitting exactly there WOULD be replaced by the directory placeholder
        // `restore` creates for a `160000` entry (ADR-MG-01).
        for path in gitlink_paths {
            if untracked == path {
                return Err(PullMergeError::UntrackedOverwrite {
                    path: untracked.display().to_string(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn write_workdir_file(
    workdir: &Path,
    relative: &Path,
    content: &[u8],
) -> Result<(), String> {
    write_workdir_file_with_mode(workdir, relative, content, false)
}

pub(crate) fn write_workdir_file_with_mode(
    workdir: &Path,
    relative: &Path,
    content: &[u8],
    executable: bool,
) -> Result<(), String> {
    let file_path = workdir.join(relative);
    if let Some(parent) = file_path.parent() {
        clear_ancestor_files(workdir, relative)?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    refuse_symlink_components(workdir, relative)?;
    clear_write_target(&file_path)?;
    crate::utils::worktree_blob::write_worktree_blob(&file_path, content, executable)
        .map_err(|error| format!("failed to write {}: {error}", file_path.display()))
}

pub(crate) fn clear_ancestor_files(workdir: &Path, relative: &Path) -> Result<(), String> {
    let mut ancestors: Vec<&Path> = relative.ancestors().skip(1).collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let full = workdir.join(ancestor);
        match fs::symlink_metadata(&full) {
            Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => {
                fs::remove_file(&full).map_err(|error| {
                    format!(
                        "failed to replace the file {} with a directory: {error}",
                        ancestor.display()
                    )
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn clear_write_target(full: &Path) -> Result<(), String> {
    let Ok(meta) = fs::symlink_metadata(full) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return fs::remove_file(full).map_err(|error| {
            format!(
                "failed to replace symbolic link {}: {error}",
                full.display()
            )
        });
    }
    if meta.is_dir() {
        return remove_empty_dir_tree(full).map_err(|error| {
            format!(
                "failed to replace directory {} with a file: {error}",
                full.display()
            )
        });
    }
    // A regular file is UNLINKED, never truncated in place: Git replaces the
    // directory entry (verified — after `git merge` rewrites a tracked file
    // its inode changes and a hard-linked alias elsewhere keeps the old
    // content and mode), so writing through the old inode would corrupt such
    // an alias and keep stale permissions.
    fs::remove_file(full).map_err(|error| format!("failed to replace {}: {error}", full.display()))
}

pub(crate) fn refuse_symlink_components(workdir: &Path, relative: &Path) -> Result<(), String> {
    let mut current = relative.parent();
    while let Some(dir) = current {
        if dir.as_os_str().is_empty() {
            break;
        }
        if fs::symlink_metadata(workdir.join(dir)).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(format!(
                "refusing to write '{}' through the symbolic link '{}'",
                relative.display(),
                dir.display()
            ));
        }
        current = dir.parent();
    }
    Ok(())
}

pub(crate) fn refuse_symlink_traversal(
    workdir: &Path,
    writes: &[PathBuf],
    removals: &[PathBuf],
) -> Result<(), PullMergeError> {
    let removed: HashSet<&PathBuf> = removals.iter().collect();
    let mut cleared: HashSet<PathBuf> = HashSet::new();
    for path in writes.iter().chain(removals) {
        let mut current = path.parent();
        while let Some(dir) = current {
            if dir.as_os_str().is_empty() || cleared.contains(dir) {
                break;
            }
            if !removed.contains(&dir.to_path_buf())
                && fs::symlink_metadata(workdir.join(dir))
                    .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                return Err(PullMergeError::WorkdirReset(format!(
                    "refusing to touch '{}' through the symbolic link '{}'",
                    path.display(),
                    dir.display()
                )));
            }
            cleared.insert(dir.to_path_buf());
            current = dir.parent();
        }
    }
    // A path the merge writes as a FILE while the working tree has a directory
    // there (MG-04: `foo/` giving way back to the file `foo`) is taken over
    // only when nothing but this merge's own removals lives inside it —
    // checked here, before the first mutation, instead of failing mid-write.
    for path in writes {
        let full = workdir.join(path);
        if !fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        if let Some(blocker) = directory_content_outside(&full, workdir, &removed)? {
            return Err(PullMergeError::WorkdirReset(format!(
                "refusing to replace directory '{}' with a file: '{}' is in the way",
                path.display(),
                blocker.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn directory_content_outside(
    dir: &Path,
    workdir: &Path,
    removed: &HashSet<&PathBuf>,
) -> Result<Option<PathBuf>, PullMergeError> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).map_err(|error| {
            PullMergeError::WorkdirReset(format!(
                "failed to inspect {}: {error}",
                current.display()
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to inspect {}: {error}",
                    current.display()
                ))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to inspect {}: {error}",
                    entry.path().display()
                ))
            })?;
            if file_type.is_dir() && !file_type.is_symlink() {
                stack.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(workdir)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| entry.path());
            if !removed.contains(&relative) {
                return Ok(Some(relative));
            }
        }
    }
    Ok(None)
}

pub(crate) fn remove_empty_dir_tree(dir: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is a symbolic link",
            dir.display()
        )));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            remove_empty_dir_tree(&entry.path())?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} is not empty ({} is in the way)",
                dir.display(),
                entry.path().display()
            )));
        }
    }
    fs::remove_dir(dir)
}

pub(crate) fn prune_empty_parents(workdir: &Path, relative: &Path) {
    let mut current = relative.parent();
    while let Some(dir) = current {
        if dir.as_os_str().is_empty() || fs::remove_dir(workdir.join(dir)).is_err() {
            break;
        }
        current = dir.parent();
    }
}

pub(crate) fn reset_workdir_tracked_only(
    current_index: &Index,
    new_index: &Index,
) -> Result<(), PullMergeError> {
    let workdir = util::working_dir();
    let untracked_paths =
        worktree::untracked_workdir_paths(current_index).map_err(PullMergeError::IndexLoad)?;
    if let Some(conflict) = worktree::untracked_overwrite_path(&untracked_paths, new_index) {
        return Err(PullMergeError::UntrackedOverwrite {
            path: conflict.display().to_string(),
        });
    }

    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();
    let writes: Vec<PathBuf> = new_tracked_paths
        .iter()
        .filter(|path| !is_gitlink_index_path(new_index, path).unwrap_or(false))
        .cloned()
        .collect();
    let removals: Vec<PathBuf> = current_index
        .tracked_files()
        .into_iter()
        .filter(|path| !new_tracked_paths.contains(path))
        .filter(|path| !is_gitlink_index_path(current_index, path).unwrap_or(false))
        .collect();
    refuse_symlink_traversal(&workdir, &writes, &removals)?;
    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            // A submodule directory is not Libra's to delete, and a gitlink can
            // only leave the index through a decision the ADR-MG-01 guard
            // already refused — so never unlink one here.
            if is_gitlink_index_path(current_index, &path_buf)? {
                continue;
            }
            let full_path = workdir.join(&path_buf);
            // `exists()` FOLLOWS symlinks, so a dangling tracked link would
            // survive and then block the write of a path beneath it (MG-04: a
            // tracked symlink `foo` giving way to the directory `foo/`).
            if fs::symlink_metadata(&full_path).is_ok() {
                fs::remove_file(&full_path).map_err(|error| {
                    PullMergeError::WorkdirReset(format!("failed to remove file: {error}"))
                })?;
                prune_empty_parents(&workdir, &path_buf);
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        if let Some(entry) = new_index.get(path_to_index_key(&path_buf)?, 0) {
            // Pass-through gitlink: nothing to materialize in the working tree.
            if entry.mode & 0o170000 == 0o160000 {
                continue;
            }
            // A path whose result already matches HEAD need not be rewritten;
            // preserving its mtime also preserves unrelated working-tree edits.
            if let Some(current) = current_index.get(path_to_index_key(&path_buf)?, 0)
                && current.hash == entry.hash
                && current.mode == entry.mode
            {
                continue;
            }
            let blob: Blob = load_object(&entry.hash).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to load blob {} for '{}': {error}",
                    entry.hash,
                    path_buf.display()
                ))
            })?;
            write_workdir_entry(
                &workdir,
                &path_buf,
                index_mode_to_tree_item_mode(entry.mode)?,
                &blob.data,
            )
            .map_err(PullMergeError::WorkdirReset)?;
        }
    }
    Ok(())
}
