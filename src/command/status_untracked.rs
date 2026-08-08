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
    metadata: &std::fs::Metadata,
    index_file_mtime: Option<std::time::SystemTime>,
) -> bool {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use git_internal::internal::index::Time;

    // Mirrors git-internal's private `index_ctime`/`index_mtime`/
    // `unix_metadata_time` so the comparison stays byte-identical to
    // `Index::is_modified`; only the extra stat (and its `unwrap`) is gone.
    #[cfg(unix)]
    fn stat_times(metadata: &std::fs::Metadata) -> (SystemTime, SystemTime) {
        use std::os::unix::fs::MetadataExt;

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
            at(metadata.ctime(), metadata.ctime_nsec()),
            at(metadata.mtime(), metadata.mtime_nsec()),
        )
    }
    #[cfg(not(unix))]
    fn stat_times(metadata: &std::fs::Metadata) -> (SystemTime, SystemTime) {
        let _ = Duration::from_secs(0);
        (
            metadata
                .created()
                .or_else(|_| metadata.modified())
                .unwrap_or(UNIX_EPOCH),
            metadata
                .modified()
                .or_else(|_| metadata.created())
                .unwrap_or(UNIX_EPOCH),
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
        let stat_path = file_abs.clone();
        let stat = match crate::command::status_probe::with_io_deadline(move || {
            stat_path.symlink_metadata()
        }) {
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
            let hash_path = file_abs.clone();
            let hashed = match crate::command::status_probe::with_io_deadline(move || {
                calc_file_blob_hash(&hash_path)
            }) {
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
    let mut scan = WorkdirScan {
        untracked: Vec::new(),
        ignored: Vec::new(),
        io_blocked: Vec::new(),
    };
    let mut pending_dirs = vec![workdir.to_path_buf()];

    while let Some(dir) = pending_dirs.pop() {
        let dir_rel = dir.strip_prefix(workdir).unwrap_or(&dir).to_path_buf();
        // §B.3.3: opening a directory on a stalled mount must reclaim the
        // caller, not wedge the whole scan. The listing itself is collected
        // inside the deadline for the same reason — a `ReadDir` iterator
        // performs further syscalls as it advances, so returning the lazy
        // iterator would move the blocking work back outside the budget.
        // `DirEntry::file_type()` is resolved inside the worker too: on
        // filesystems without d_type it is a stat, so leaving it outside
        // would put a blocking syscall per entry beyond the deadline.
        let open_dir = dir.clone();
        // The worker publishes entries into a shared buffer as it goes, so a
        // deadline hit KEEPS everything already collected instead of
        // discarding a large directory that was making steady progress. That
        // is what the "no-progress deadline" means for the main scan, whose
        // contract is to report every `??` it can see.
        type ScanEntry = (PathBuf, std::ffi::OsString, io::Result<std::fs::FileType>);
        let collected: std::sync::Arc<std::sync::Mutex<Vec<ScanEntry>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&collected);
        // The deadline measures LACK of progress, not total time: a
        // directory with a million entries that keeps yielding them is not a
        // hung mount, and reporting it as `io_timeout` would silently drop
        // untracked files the scan is contractually required to list.
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticker = std::sync::Arc::clone(&progress);
        let listing = match crate::command::status_probe::with_no_progress_deadline(
            std::sync::Arc::clone(&progress),
            move || {
                std::fs::read_dir(&open_dir).map(|reader| {
                    // Debug-only seam companion: replace ONE yielded entry
                    // with `Err(NotFound)` so the vanished-entry skip arm
                    // below is exercisable (a real vanish between readdir
                    // batches cannot be staged from a test).
                    #[cfg(debug_assertions)]
                    let mut injected_notfound = false;
                    for entry in reader {
                        #[cfg(debug_assertions)]
                        let entry = if !injected_notfound
                            && std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
                            && std::env::var("LIBRA_TEST_READDIR_ENTRY_NOTFOUND_DIR")
                                .is_ok_and(|target| open_dir.ends_with(&target))
                        {
                            injected_notfound = true;
                            Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                "injected vanished entry",
                            ))
                        } else {
                            entry
                        };
                        // `file_type()` can be a stat on filesystems without
                        // d_type, so it is resolved BEFORE the lock is taken:
                        // a reclaimed caller must be able to drain what has
                        // been collected, and it cannot if the worker is
                        // parked in a syscall while holding the buffer.
                        let entry = match entry {
                            Ok(entry) => entry,
                            // §B.3.3 "entry NotFound → continue": ONE entry
                            // vanishing mid-iteration is not a failed
                            // listing — skip it and keep iterating, never
                            // abandon the directory's remaining entries.
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(error),
                        };
                        let record = (entry.path(), entry.file_name(), entry.file_type());
                        sink.lock().unwrap_or_else(|e| e.into_inner()).push(record);
                        ticker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // Debug-only seam: force a mid-iteration
                        // `ReadDir::next` error — the errno-passthrough
                        // class (NFS/FUSE readdir failures) that no tempdir
                        // fixture can produce. The error KIND is
                        // injectable so the genuine-`TimedOut` mapping is
                        // testable too. Gated on `LIBRA_TEST` like the
                        // rest of the seam family.
                        #[cfg(debug_assertions)]
                        if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
                            && std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_DIR")
                                .is_ok_and(|target| open_dir.ends_with(&target))
                        {
                            let kind = match std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_KIND")
                                .as_deref()
                            {
                                Ok("timedout") => io::ErrorKind::TimedOut,
                                _ => io::ErrorKind::Other,
                            };
                            return Err(io::Error::new(
                                kind,
                                "injected mid-iteration readdir error",
                            ));
                        }
                    }
                    Ok::<(), io::Error>(())
                })
            },
        ) {
            Err(()) => {
                // Reclaimed by the watchdog: record the block HERE and
                // resolve to `Ok(())` so no sentinel error value needs a
                // filtering arm below — a filtering arm keyed on the error
                // KIND would also swallow a genuine `TimedOut` from the
                // listing itself (2026-08-05 R0-3 review).
                scan.io_blocked.push(io_timeout_event(&dir_rel));
                Ok(())
            }
            // Flatten BOTH layers: the outer error is the `read_dir` open
            // failure, the inner one a mid-iteration `ReadDir::next`
            // failure. Discarding the inner error would claim a complete
            // scan while this directory's remaining entries were silently
            // dropped — a fail-open hole in the §B.3.3 accumulator
            // contract (2026-08-05 R0-3 review).
            Ok(result) => result.and_then(|iteration| iteration),
        };
        match listing {
            Ok(()) => {}
            // The directory itself vanished before it could be opened:
            // nothing to report, nothing was collected for it.
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                // §B.3.3: record and continue with the remaining pending
                // directories — never drop what earlier siblings collected,
                // nor what this directory already yielded.
                // (`io_blocked_event` maps a genuine `TimedOut` to the
                // `IoTimeout` reason.)
                scan.io_blocked.push(io_blocked_event(&dir_rel, &source));
            }
        }
        let reader = {
            let mut buffer = collected.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *buffer)
        };
        for (path, name, file_type) in reader {
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }

            let entry_rel = path.strip_prefix(workdir).unwrap_or(&path).to_path_buf();
            let file_type = match file_type {
                Ok(file_type) => file_type,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    scan.io_blocked.push(io_blocked_event(&entry_rel, &source));
                    continue;
                }
            };
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| list_error(&dir, io::Error::other(err.to_string())))?
                .to_path_buf();
            if file_type.is_dir() {
                // The ignore lookup reads `.gitignore`/`.libraignore` files,
                // so it is a filesystem operation like any other and runs
                // under the deadline. A reclaimed lookup is treated as
                // "ignored" (conservative: it never invents an untracked
                // entry) and the directory is reported blocked.
                // Resolve the DB-backed `core.excludesFile` here, not on the
                // worker: a database read charged against the worktree I/O
                // deadline reads back as an `io_timeout` on a fine path.
                util::prewarm_ignore_config(workdir);
                let ignore_target = (workdir.to_path_buf(), path.clone());
                let ignored = match crate::command::status_probe::with_io_deadline(move || {
                    let (workdir, path) = ignore_target;
                    util::check_gitignore(&workdir, &path)
                }) {
                    Ok(value) => value,
                    Err(()) => {
                        scan.io_blocked.push(io_timeout_event(&entry_rel));
                        true
                    }
                };
                if ignored {
                    if include_ignored {
                        // Mirror the untracked branch (§B.3.5): an ignored
                        // DIRECTORY is reported with its `/` marker, never
                        // as a file-shaped path.
                        scan.ignored.push(directory_marker(&relative));
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
                    match untracked_dir_visibility(workdir, &path) {
                        DirVisibility::HasVisibleFile => {
                            scan.untracked.push(directory_marker(&relative));
                        }
                        DirVisibility::Empty => {}
                        DirVisibility::Blocked(error) => {
                            // §B.3.3: we could not verify the directory's
                            // contents. Report the marker (conservative,
                            // matching git) AND record the block so the
                            // base scan is not claimed complete and the
                            // dirty cache is not rewritten from a guess.
                            //
                            // The block is ABSORBED in the listing sense:
                            // the marker is in the output, so over-reporting
                            // (a readable scan might hide the dir) is the
                            // safe direction. §B.6.0.1 still fails text
                            // modes closed — a marker is not an inspection
                            // result — while JSON keeps the partial
                            // contract with `base_scan_complete` false.
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
    // §B.3.3: this probe walks a subtree with `read_dir` + per-entry
    // metadata, all of which block on a stalled mount. One deadline wraps
    // the WHOLE walk rather than each syscall: the contract is a
    // no-progress bound on the operation, and per-entry hops would add a
    // worker round-trip to every file in a wide tree.
    let workdir = workdir.to_path_buf();
    let dir = dir.to_path_buf();
    // No-progress, not wall-clock: a large untracked tree that keeps
    // yielding entries is not a hung mount, and reporting it as `io_timeout`
    // would fail text status and truncate the JSON for a directory that was
    // perfectly readable.
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ticker = std::sync::Arc::clone(&progress);
    match crate::command::status_probe::with_no_progress_deadline(
        std::sync::Arc::clone(&progress),
        move || untracked_dir_visibility_blocking(&workdir, &dir, &ticker),
    ) {
        Ok(visibility) => visibility,
        Err(()) => DirVisibility::Blocked(io::Error::new(
            io::ErrorKind::TimedOut,
            "directory listing exceeded the status I/O deadline",
        )),
    }
}

fn untracked_dir_visibility_blocking(
    workdir: &Path,
    dir: &Path,
    progress: &std::sync::atomic::AtomicUsize,
) -> DirVisibility {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) => return DirVisibility::Blocked(error),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => return DirVisibility::Blocked(error),
            };
            progress.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let name = entry.file_name();
            if name == OsStr::new(util::ROOT_DIR) || name == OsStr::new(util::GIT_DIR) {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => return DirVisibility::Blocked(error),
            };
            let path = entry.path();
            match util::check_gitignore_bounded(workdir, &path) {
                Some(true) => continue,
                Some(false) => {}
                // Undecidable: "cannot tell" is not "empty" — report the
                // directory blocked so its marker is conservatively kept.
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
            if file_type.is_dir() {
                pending.push(path);
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
