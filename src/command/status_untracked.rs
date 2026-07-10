use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use git_internal::internal::index::Index;

use super::{
    calc_file_blob_hash,
    status::{Changes, StatusError, UntrackedFiles},
    status_untracked_paths::{
        TrackedPaths, collapse_untracked_directories, directory_marker, is_top_level_path,
        sort_paths,
    },
};
use crate::utils::{path, util};

pub(crate) struct StatusWorktreeChanges {
    pub(crate) unstaged: Changes,
    pub(crate) ignored_files: Vec<PathBuf>,
    pub(crate) index: Index,
}

struct WorkdirScan {
    untracked: Vec<PathBuf>,
    ignored: Vec<PathBuf>,
}

pub(crate) fn collect_status_worktree_changes(
    untracked_mode: UntrackedFiles,
    include_ignored: bool,
    ignore_case: bool,
) -> Result<StatusWorktreeChanges, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let tracked = TrackedPaths::from_index(&index, ignore_case);
    let mut unstaged = collect_tracked_worktree_changes(&workdir, &index, tracked.files())?;
    let mut ignored_files = Vec::new();

    if !matches!(untracked_mode, UntrackedFiles::No) {
        let scan = scan_workdir(&workdir, &index, &tracked, untracked_mode, include_ignored)?;
        unstaged.new = if matches!(untracked_mode, UntrackedFiles::Normal) {
            collapse_untracked_directories(scan.untracked, &tracked)
        } else {
            sort_paths(scan.untracked)
        };
        ignored_files = if matches!(untracked_mode, UntrackedFiles::Normal) {
            collapse_untracked_directories(scan.ignored, &tracked)
        } else {
            sort_paths(scan.ignored)
        };
    }

    Ok(StatusWorktreeChanges {
        unstaged,
        ignored_files,
        index,
    })
}

pub(crate) fn changes_to_current_directory(mut changes: Changes) -> Changes {
    changes.new = changes
        .new
        .into_iter()
        .map(path_to_current_preserving_directory_marker)
        .collect();
    changes.modified = changes
        .modified
        .into_iter()
        .map(util::workdir_to_current)
        .collect();
    changes.deleted = changes
        .deleted
        .into_iter()
        .map(util::workdir_to_current)
        .collect();
    changes.renamed = changes
        .renamed
        .into_iter()
        .map(|(old, new)| (util::workdir_to_current(old), util::workdir_to_current(new)))
        .collect();
    changes
}

fn path_to_current_preserving_directory_marker(path: PathBuf) -> PathBuf {
    if !path.to_string_lossy().ends_with('/') {
        return util::workdir_to_current(path);
    }

    let relative = util::workdir_to_current(&path);
    directory_marker(&relative)
}

fn collect_tracked_worktree_changes(
    workdir: &Path,
    index: &Index,
    tracked_files: &[PathBuf],
) -> Result<Changes, StatusError> {
    let mut changes = Changes::default();
    for file in tracked_files {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        let file_abs = workdir.join(file);
        if file_abs.symlink_metadata().is_err() {
            changes.deleted.push(file.clone());
        } else if index.is_modified(file_str, 0, workdir) {
            let file_hash =
                calc_file_blob_hash(&file_abs).map_err(|source| StatusError::FileHash {
                    path: file_abs.clone(),
                    source,
                })?;
            if !index.verify_hash(file_str, 0, &file_hash) {
                changes.modified.push(file.clone());
            }
        }
    }
    Ok(changes)
}

fn scan_workdir(
    workdir: &Path,
    index: &Index,
    tracked: &TrackedPaths,
    untracked_mode: UntrackedFiles,
    include_ignored: bool,
) -> Result<WorkdirScan, StatusError> {
    let mut scan = WorkdirScan {
        untracked: Vec::new(),
        ignored: Vec::new(),
    };
    let mut pending_dirs = vec![workdir.to_path_buf()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|source| list_error(&dir, source))? {
            let entry = entry.map_err(|source| list_error(&dir, source))?;
            let path = entry.path();
            let name = entry.file_name();
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }

            let file_type = entry
                .file_type()
                .map_err(|source| list_error(&dir, source))?;
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| list_error(&dir, io::Error::other(err.to_string())))?
                .to_path_buf();
            if file_type.is_dir() {
                if util::check_gitignore(workdir, &path) {
                    if include_ignored {
                        scan.ignored.push(relative);
                    }
                    continue;
                }
                if matches!(untracked_mode, UntrackedFiles::Normal)
                    && !include_ignored
                    && is_top_level_path(&relative)
                    && !tracked.has_descendant(&relative)
                {
                    // Git only reports an untracked directory when it holds
                    // at least one visible untracked file. A directory whose
                    // entire contents are skip-listed (`.libra`/`.git`) or
                    // ignored must stay invisible — e.g. test harnesses'
                    // `.libra-test-home/` holding only a nested `.libra`.
                    // The probe stops at the first qualifying file, so the
                    // no-descend perf win survives for real content; an
                    // unreadable directory is conservatively reported (we
                    // cannot verify it is empty, and git reports it too).
                    if untracked_dir_has_visible_file(workdir, &path) {
                        scan.untracked.push(directory_marker(&relative));
                    }
                    continue;
                }
                pending_dirs.push(path);
            } else if file_type.is_file() || file_type.is_symlink() {
                scan_file(
                    &mut scan,
                    workdir,
                    index,
                    tracked,
                    &path,
                    &relative,
                    include_ignored,
                )?;
            }
        }
    }

    Ok(scan)
}

fn scan_file(
    scan: &mut WorkdirScan,
    workdir: &Path,
    index: &Index,
    tracked_paths: &TrackedPaths,
    path: &Path,
    relative: &Path,
    include_ignored: bool,
) -> Result<(), StatusError> {
    let file_str = relative
        .to_str()
        .ok_or_else(|| StatusError::InvalidPathEncoding {
            path: relative.to_path_buf(),
        })?;
    let tracked =
        index.tracked(file_str, 0) || tracked_paths.same_file_case_alias(workdir, relative);
    if util::check_gitignore(workdir, path) {
        if include_ignored && !tracked {
            scan.ignored.push(relative.to_path_buf());
        }
    } else if !tracked {
        scan.untracked.push(relative.to_path_buf());
    }
    Ok(())
}

/// Whether an untracked top-level directory contains at least one visible
/// (non-skip-listed, non-ignored) file — the git precondition for showing
/// the collapsed `dir/` marker. Walks lazily and returns at the first hit;
/// any read error makes the answer `true` (report rather than silently
/// hide something we could not inspect).
fn untracked_dir_has_visible_file(workdir: &Path, dir: &Path) -> bool {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => return true,
        };
        for entry in entries {
            let Ok(entry) = entry else { return true };
            let name = entry.file_name();
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                return true;
            };
            let path = entry.path();
            if util::check_gitignore(workdir, &path) {
                continue;
            }
            if file_type.is_file() || file_type.is_symlink() {
                return true;
            }
            if file_type.is_dir() {
                pending.push(path);
            }
        }
    }
    false
}

fn list_error(path: &Path, source: io::Error) -> StatusError {
    StatusError::ListWorkdirFiles {
        path: path.to_path_buf(),
        source,
    }
}
