use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use git_internal::internal::index::Index;

use super::{
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
    /// §B.3.3 accumulator protocol: paths the scan could not inspect
    /// (workdir-relative). Text formats fail closed on any entry; JSON
    /// reports the partial result plus `data.io_blocked[]`.
    pub(crate) io_blocked: Vec<crate::command::status_probe::IoBlockedEvent>,
}

struct WorkdirScan {
    untracked: Vec<PathBuf>,
    ignored: Vec<PathBuf>,
    io_blocked: Vec<crate::command::status_probe::IoBlockedEvent>,
}

/// A path whose inspection exceeded the per-operation deadline (§B.3.3).
/// Distinct from a plain I/O error so consumers can tell "the mount is
/// hung" apart from "the read failed".
fn io_timeout_event(path: &Path) -> crate::command::status_probe::IoBlockedEvent {
    crate::command::status_probe::IoBlockedEvent {
        path: path.to_path_buf(),
        reason: crate::command::status_probe::IoBlockedReason::IoTimeout,
        absorbed: false,
    }
}

fn io_blocked_event(
    path: &Path,
    error: &io::Error,
) -> crate::command::status_probe::IoBlockedEvent {
    use crate::command::status_probe::{IoBlockedEvent, IoBlockedReason};
    IoBlockedEvent {
        path: path.to_path_buf(),
        // `TimedOut` is its own reason in the public taxonomy: a reclaimed
        // deadline means "the mount is hung", which callers act on
        // differently from an ordinary read failure. Folding it into
        // `IoError` would silently break the documented enum.
        reason: match error.kind() {
            io::ErrorKind::PermissionDenied => IoBlockedReason::PermissionDenied,
            io::ErrorKind::TimedOut => IoBlockedReason::IoTimeout,
            _ => IoBlockedReason::IoError,
        },
        absorbed: false,
    }
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
    let mut io_blocked: Vec<crate::command::status_probe::IoBlockedEvent> = Vec::new();
    // The index file's own mtime anchors the racily-clean guard: a stat
    // triple is only trusted for files strictly older than this snapshot.
    let index_file_mtime = std::fs::metadata(&index_path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let mut unstaged = collect_tracked_worktree_changes(
        &workdir,
        &index,
        tracked.files(),
        &mut io_blocked,
        index_file_mtime,
    )?;
    let mut ignored_files = Vec::new();

    if !matches!(untracked_mode, UntrackedFiles::No) {
        let mut scan = scan_workdir(&workdir, &index, &tracked, untracked_mode, include_ignored)?;
        io_blocked.append(&mut scan.io_blocked);
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

    io_blocked.sort_by_key(|event| crate::command::status::raw_path_sort_key(&event.path));
    io_blocked.dedup_by(|a, b| a.path == b.path);
    Ok(StatusWorktreeChanges {
        unstaged,
        ignored_files,
        index,
        io_blocked,
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

/// Whether the index entry's cached stat data disagrees with `metadata`
/// (the `ctime`/`mtime`/`size` triple `Index::is_modified` compares). A path
/// missing from the index counts as differing so the caller falls through to
/// the content hash rather than assuming "clean".
fn index_stat_differs(
    index: &Index,
    file: &str,
    metadata: &crate::command::status_io_worker::CapturedStat,
    index_file_mtime: Option<std::time::SystemTime>,
) -> bool {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use git_internal::internal::index::Time;

    // Mirrors git-internal's private `index_ctime`/`index_mtime`/
    // `unix_metadata_time` so the comparison stays byte-identical to
    // `Index::is_modified`; only the extra stat (and its `unwrap`) is gone.
    fn stat_times(
        metadata: &crate::command::status_io_worker::CapturedStat,
    ) -> (SystemTime, SystemTime) {
        fn at(seconds: i64, nanos: i64) -> SystemTime {
            if seconds < 0 {
                return UNIX_EPOCH;
            }
            let nanos = u32::try_from(nanos)
                .ok()
                .filter(|nanos| *nanos < 1_000_000_000)
                .unwrap_or(0);
            UNIX_EPOCH + Duration::new(seconds as u64, nanos)
        }
        (
            at(metadata.ctime_sec, metadata.ctime_nsec),
            at(metadata.mtime_sec, metadata.mtime_nsec),
        )
    }

    let Some(entry) = index.get(file, 0) else {
        return true;
    };
    let Ok(stat_size) = u32::try_from(metadata.len()) else {
        // A >4GiB stat size cannot be represented in the entry; the old
        // truncating cast could collide with a smaller recorded size —
        // always content-compare instead (2026-08-06 R0-8 review, the
        // guarded comparison diff already used).
        return true;
    };
    let (ctime, mtime) = stat_times(metadata);
    let same = entry.ctime == Time::from_system_time(ctime)
        && entry.mtime == Time::from_system_time(mtime)
        && entry.size == stat_size;
    if same {
        // Racily-clean guard (Git parity, §B.6.0.1): a matching stat
        // triple is trustworthy only when the file is strictly OLDER than
        // the index snapshot itself. An entry written in the same instant
        // the file changed can pair a post-edit stat with a pre-edit hash
        // — the 2026-08-06 incident where plain status hid a modified
        // CHANGELOG.md that diff's stricter shortcut still caught (the
        // index writers stat AFTER hashing, and a concurrent node shares
        // this worktree). In the match branch entry.mtime equals the
        // worktree mtime, so the ordering runs on the worktree SystemTime
        // (finer precision, conservative direction). An unknown index
        // mtime never earns trust.
        let trustworthy = index_file_mtime.is_some_and(|snapshot| mtime < snapshot);
        return !trustworthy;
    }
    true
}

fn collect_tracked_worktree_changes(
    workdir: &Path,
    index: &Index,
    tracked_files: &[PathBuf],
    io_blocked: &mut Vec<crate::command::status_probe::IoBlockedEvent>,
    index_file_mtime: Option<std::time::SystemTime>,
) -> Result<Changes, StatusError> {
    let mut changes = Changes::default();
    for file in tracked_files {
        // §B.6.1: an index key is a UTF-8 string by construction, so this
        // only fires if a path arrives from somewhere else. Skip the KEYED
        // comparisons rather than failing the whole status — the contract is
        // that a non-UTF-8 name costs rename candidacy, never the command.
        let Some(file_str) = file.to_str() else {
            continue;
        };
        // ADR-SW-04 item 1: a skip-worktree entry is a sparse-checkout path
        // whose worktree copy may legitimately be absent or stale; it is
        // never a status change.
        if index
            .get(file_str, 0)
            .is_some_and(|entry| entry.flags.skip_worktree)
        {
            continue;
        }
        // A gitlink (mode 0o160000) records a submodule COMMIT, not a blob:
        // hashing the directory as file content would fail and be reported
        // as an unreadable path. Submodule status is out of R0 scope, so the
        // entry is skipped rather than mis-reported.
        if index
            .get(file_str, 0)
            .is_some_and(|entry| entry.mode == 0o160000)
        {
            continue;
        }
        let file_abs = workdir.join(file);
        // A tracked path that is genuinely gone is a deletion; but a real I/O
        // error (permission denied, I/O failure) only means "can't tell" — it
        // must NOT be reported as a deletion NOR fabricated as clean
        // (§B.6.0.1/§B.3.3): record the event, keep everything already
        // collected, and continue with the remaining tracked files. Text
        // formats fail closed at render time; JSON reports the partial
        // result with `data.io_blocked[]`.
        // §B.3.3: both the stat and the content read run under the
        // per-operation deadline. A hung mount (or a FIFO left in the tree)
        // must reclaim the caller and report the path blocked — never wedge
        // `status` forever, and never let the timeout read as "clean".
        let stat = match crate::command::status_io_worker::deadline_stat(&file_abs) {
            Ok(result) => result,
            Err(()) => {
                io_blocked.push(io_timeout_event(file));
                continue;
            }
        };
        let metadata = match stat {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                changes.deleted.push(file.clone());
                continue;
            }
            Err(source) => {
                io_blocked.push(io_blocked_event(file, &source));
                continue;
            }
            Ok(metadata) => metadata,
        };
        // Compare against the metadata we ALREADY hold rather than calling
        // `Index::is_modified`, which re-stats the path and `unwrap()`s the
        // result: a path deleted or made unreadable between the two stats
        // would panic the whole command instead of degrading to an
        // `io_blocked[]` partial. Same fields, one stat, no panic.
        if index_stat_differs(index, file_str, &metadata, index_file_mtime) {
            let hashed =
                match crate::command::status_io_worker::deadline_file_blob_hash(&file_abs, workdir)
                {
                    Ok(result) => result,
                    Err(()) => {
                        io_blocked.push(io_timeout_event(file));
                        continue;
                    }
                };
            match hashed {
                Ok(file_hash) => {
                    if !index.verify_hash(file_str, 0, &file_hash) {
                        changes.modified.push(file.clone());
                    }
                }
                Err(source) => {
                    io_blocked.push(io_blocked_event(file, &source));
                    continue;
                }
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
    let _ignore_walk = util::begin_secure_ignore_walk();
    let mut scan = WorkdirScan {
        untracked: Vec::new(),
        ignored: Vec::new(),
        io_blocked: Vec::new(),
    };
    let mut pending_dirs = vec![workdir.to_path_buf()];

    while let Some(dir) = pending_dirs.pop() {
        let dir_rel = dir.strip_prefix(workdir).unwrap_or(&dir).to_path_buf();
        // Revalidate the directory we are about to classify children of.
        // Ignore sources for those children include `dir/.gitignore` via
        // pathname I/O; a post-listing swap must not redirect that read.
        if dir != workdir {
            match crate::command::status_io_worker::deadline_marker_probe(&dir) {
                Err(()) => {
                    // Unreadable revalidation: keep the collapsed `dir/`
                    // marker and record the block (same contract as
                    // DirVisibility::Blocked). Dropping the marker made
                    // status look like the directory was absent.
                    scan.untracked.push(directory_marker(&dir_rel));
                    let mut event = io_timeout_event(&dir_rel);
                    event.absorbed = true;
                    scan.io_blocked.push(event);
                    continue;
                }
                Ok(Err(source)) => {
                    scan.untracked.push(directory_marker(&dir_rel));
                    let mut event = io_blocked_event(&dir_rel, &source);
                    event.absorbed = true;
                    scan.io_blocked.push(event);
                    continue;
                }
                Ok(Ok(true)) => {
                    // Nested repository acquired after queueing — do not scan.
                    continue;
                }
                Ok(Ok(_)) => {}
            }
        }
        // §B.3.3 / WIO-01: listing runs in the out-of-process worker. A
        // no-progress timeout keeps checkpointed entries and marks the
        // directory `IoBlocked` instead of hanging or discarding partial.
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listing = match crate::command::status_io_worker::deadline_read_dir(
            &dir,
            usize::MAX,
            &progress,
        ) {
            Err(()) => {
                if dir != workdir {
                    scan.untracked.push(directory_marker(&dir_rel));
                    let mut event = io_timeout_event(&dir_rel);
                    event.absorbed = true;
                    scan.io_blocked.push(event);
                } else {
                    scan.io_blocked.push(io_timeout_event(&dir_rel));
                }
                continue;
            }
            Ok(Err(source)) if source.kind() == io::ErrorKind::NotFound => continue,
            Ok(Err(source)) => {
                // Worktree root: never emit `/` as an untracked marker — same
                // contract as the timeout branch above.
                if dir != workdir {
                    scan.untracked.push(directory_marker(&dir_rel));
                    let mut event = io_blocked_event(&dir_rel, &source);
                    event.absorbed = true;
                    scan.io_blocked.push(event);
                } else {
                    scan.io_blocked.push(io_blocked_event(&dir_rel, &source));
                }
                continue;
            }
            Ok(Ok(listing)) => listing,
        };
        if listing.timed_out {
            scan.io_blocked.push(io_timeout_event(&dir_rel));
        } else if let Some((kind, raw_os)) = listing.error_kinds.first().copied() {
            let source = crate::command::status_io_worker::io_from_wire(kind, raw_os);
            scan.io_blocked.push(io_blocked_event(&dir_rel, &source));
        }
        for dirent in listing.entries {
            let name = crate::command::status_io_worker::dirent_os(&dirent.name);
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }

            let path = dir.join(&name);
            let entry_rel = path.strip_prefix(workdir).unwrap_or(&path).to_path_buf();
            let file_type =
                match crate::command::status_io_worker::deadline_dirent_kind(&path, &dirent) {
                    Err(()) => {
                        scan.io_blocked.push(io_timeout_event(&entry_rel));
                        continue;
                    }
                    Ok(Err(source)) if source.kind() == io::ErrorKind::NotFound => continue,
                    Ok(Err(source)) => {
                        scan.io_blocked.push(io_blocked_event(&entry_rel, &source));
                        continue;
                    }
                    Ok(Ok(kind)) => kind,
                };
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| list_error(&dir, io::Error::other(err.to_string())))?
                .to_path_buf();
            if file_type.is_dir() {
                // Ignore pruning stays ahead of marker/open: an ignored
                // unreadable directory must not become `io_blocked`. After a
                // non-ignored decision, revalidate through the repo-root fd
                // before descending so a post-listing escape cannot poison
                // the walk.
                util::prewarm_ignore_config(workdir);
                let ignore_target = (workdir.to_path_buf(), path.clone());
                let walk_epoch = util::secure_ignore_walk_epoch();
                let ignored = match crate::command::status_probe::with_io_deadline(move || {
                    let (workdir, path) = ignore_target;
                    util::check_gitignore_as_dir_for_walk(&workdir, &path, true, walk_epoch)
                }) {
                    Ok(value) => {
                        if util::ignore_read_failed() {
                            scan.io_blocked.push(io_blocked_event(
                                &entry_rel,
                                &io::Error::other("ignore source unreadable"),
                            ));
                            true
                        } else {
                            value
                        }
                    }
                    Err(()) => {
                        scan.io_blocked.push(io_timeout_event(&entry_rel));
                        true
                    }
                };
                if ignored {
                    if include_ignored {
                        scan.ignored.push(directory_marker(&relative));
                    }
                    continue;
                }
                match crate::command::status_io_worker::deadline_marker_probe(&path) {
                    Err(()) => {
                        scan.untracked.push(directory_marker(&relative));
                        let mut event = io_timeout_event(&relative);
                        event.absorbed = true;
                        scan.io_blocked.push(event);
                        continue;
                    }
                    Ok(Err(source)) => {
                        scan.untracked.push(directory_marker(&relative));
                        let mut event = io_blocked_event(&relative, &source);
                        event.absorbed = true;
                        scan.io_blocked.push(event);
                        continue;
                    }
                    Ok(Ok(true)) => {
                        // Nested repository: never descend. Still report the
                        // outer leaf when it holds visible non-metadata files
                        // (metadata-only nests stay invisible under Normal).
                        if matches!(untracked_mode, UntrackedFiles::Normal)
                            && !include_ignored
                            && is_top_level_path(&relative)
                            && !tracked.has_descendant(&relative)
                        {
                            match untracked_dir_visibility(workdir, &path) {
                                DirVisibility::HasVisibleFile => {
                                    scan.untracked.push(directory_marker(&relative));
                                }
                                DirVisibility::Empty => {}
                                DirVisibility::Blocked(error) => {
                                    scan.untracked.push(directory_marker(&relative));
                                    let mut event = io_blocked_event(&relative, &error);
                                    event.absorbed = true;
                                    scan.io_blocked.push(event);
                                }
                            }
                        } else {
                            scan.untracked.push(directory_marker(&relative));
                        }
                        continue;
                    }
                    Ok(Ok(false)) => {}
                }
                if matches!(untracked_mode, UntrackedFiles::Normal)
                    && !include_ignored
                    && is_top_level_path(&relative)
                    && !tracked.has_descendant(&relative)
                {
                    match untracked_dir_visibility(workdir, &path) {
                        DirVisibility::HasVisibleFile => {
                            scan.untracked.push(directory_marker(&relative));
                        }
                        DirVisibility::Empty => {}
                        DirVisibility::Blocked(error) => {
                            scan.untracked.push(directory_marker(&relative));
                            let mut event = io_blocked_event(&relative, &error);
                            event.absorbed = true;
                            scan.io_blocked.push(event);
                        }
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
    // §B.6.1: a non-UTF-8 name can never be an index entry (index paths are
    // UTF-8 strings), so it is untracked by construction — base `??`/ignored
    // classification proceeds on the raw path and the whole status must NOT
    // fail (rename candidacy is skipped separately with a warning).
    let tracked = match relative.to_str() {
        Some(file_str) => {
            index.tracked(file_str, 0) || tracked_paths.same_file_case_alias(workdir, relative)
        }
        None => false,
    };
    // An undecidable ignore state must not silently classify the entry.
    // Report it and keep the conservative untracked reading (a path the scan
    // could not classify is visible, not invisible).
    let ignored = match util::check_gitignore_bounded(workdir, path) {
        Some(value) => value,
        None => {
            scan.io_blocked.push(io_timeout_event(relative));
            false
        }
    };
    if ignored {
        if include_ignored && !tracked {
            scan.ignored.push(relative.to_path_buf());
        }
    } else if !tracked {
        scan.untracked.push(relative.to_path_buf());
    }
    Ok(())
}

/// Three-state answer for "does this untracked top-level directory hold a
/// visible (non-skip-listed, non-ignored) file?" — the git precondition
/// for showing the collapsed `dir/` marker (§B.3.3).
enum DirVisibility {
    HasVisibleFile,
    Empty,
    /// The directory could not be inspected; the caller reports the
    /// marker conservatively AND records an `io_blocked` event so the
    /// scan is never claimed complete.
    Blocked(io::Error),
}

fn untracked_dir_visibility(workdir: &Path, dir: &Path) -> DirVisibility {
    // WIO-01: listing runs in the killable worker and already carries
    // `file_type()`; ignore lookups stay in-process. A wide tree that
    // keeps yielding entries is not a hang — `read_dir` uses a
    // no-progress timeout inside the worker pool.
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listing = match crate::command::status_io_worker::deadline_read_dir(
            &current,
            usize::MAX,
            &progress,
        ) {
            Err(()) => {
                return DirVisibility::Blocked(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "directory listing exceeded the status I/O deadline",
                ));
            }
            Ok(Err(error)) => return DirVisibility::Blocked(error),
            Ok(Ok(listing)) => listing,
        };
        if listing.timed_out {
            return DirVisibility::Blocked(io::Error::new(
                io::ErrorKind::TimedOut,
                "directory listing exceeded the status I/O deadline",
            ));
        }
        if let Some((kind, raw_os)) = listing.error_kinds.first().copied() {
            return DirVisibility::Blocked(crate::command::status_io_worker::io_from_wire(
                kind, raw_os,
            ));
        }
        for dirent in listing.entries {
            let name = crate::command::status_io_worker::dirent_os(&dirent.name);
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }
            let path = current.join(&name);
            let file_type =
                match crate::command::status_io_worker::deadline_dirent_kind(&path, &dirent) {
                    Err(()) => {
                        return DirVisibility::Blocked(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "directory listing exceeded the status I/O deadline",
                        ));
                    }
                    Ok(Err(error)) => return DirVisibility::Blocked(error),
                    Ok(Ok(kind)) => kind,
                };
            if file_type.is_dir() {
                match util::check_gitignore_bounded_as_dir(workdir, &path, true) {
                    Some(true) => continue,
                    Some(false) => {}
                    None => {
                        return DirVisibility::Blocked(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "ignore lookup exceeded the status I/O deadline",
                        ));
                    }
                }
                // Revalidate eligible directories through the repo-root fd before
                // descending; ignored paths were already pruned above.
                match crate::command::status_io_worker::deadline_marker_probe(&path) {
                    Err(()) => {
                        return DirVisibility::Blocked(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "directory revalidation exceeded the status I/O deadline",
                        ));
                    }
                    Ok(Err(error)) => return DirVisibility::Blocked(error),
                    Ok(Ok(true)) => continue, // nested metadata root — not visible content
                    Ok(Ok(false)) => {}
                }
                pending.push(path);
                continue;
            }
            match util::check_gitignore_bounded_as_dir(workdir, &path, false) {
                Some(true) => continue,
                Some(false) => {}
                None => {
                    return DirVisibility::Blocked(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "ignore lookup exceeded the status I/O deadline",
                    ));
                }
            }
            if file_type.is_file() || file_type.is_symlink() {
                return DirVisibility::HasVisibleFile;
            }
        }
    }
    DirVisibility::Empty
}

fn list_error(path: &Path, source: io::Error) -> StatusError {
    StatusError::ListWorkdirFiles {
        path: path.to_path_buf(),
        source,
    }
}
