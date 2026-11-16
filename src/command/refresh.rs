//! Shared index stat-refresh and executable-bit helpers (plan issues/479,
//! UI-02/UI-03).
//!
//! Both `add --refresh` and `update-index --refresh`/`--chmod` route through
//! this module so the stat-refresh decision and the mode flip have exactly one
//! implementation (GC-02).
//!
//! The refresh loop replicates git's `refresh_index` / `refresh_cache_ent`
//! semantics (`read-cache.c`, git 2.56): a candidate whose stat matches is
//! skipped (racy-clean entries are trusted, as in git); one whose stat changed
//! is re-hashed with the **correct blob hash** — unlike `git_internal`'s
//! `Index::refresh`, which stores the raw content hash — and either has its
//! stat fields refreshed (content still matches the staged blob) or is
//! reported as needing update (content/mode/type changed). Entries are never
//! given a hash that does not identify a real blob.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use git_internal::internal::index::{Index, IndexEntry};

use crate::utils::pathspec::PathspecSet;

/// Stat/refresh behavior switches shared by `add --refresh` (which renders its
/// own summary) and `update-index --refresh` (which renders git's
/// `needs update`/`needs merge` lines).
#[derive(Debug, Clone, Copy, Default)]
pub struct RefreshOptions {
    /// `-q`: suppress the `<path>: needs update` report and its exit code.
    /// Like git, `needs merge` is still reported.
    pub quiet: bool,
    /// `--ignore-missing`: skip entries whose worktree file is gone.
    pub ignore_missing: bool,
    /// `--unmerged`: skip unmerged entries instead of reporting them.
    pub allow_unmerged: bool,
    /// `core.fileMode` (`true` on Unix by default): when enabled, an
    /// executable-bit-only change counts as "needs update" (git never
    /// silently syncs the mode during a refresh).
    pub file_mode: bool,
}

impl RefreshOptions {
    /// The switches `add --refresh` uses: never print per-file lines (add
    /// renders its own summary) and skip unmerged entries (its candidates come
    /// from the status diff, which has no unmerged stage-0 handling).
    pub fn for_add_refresh(file_mode: bool) -> Self {
        Self {
            quiet: true,
            allow_unmerged: true,
            file_mode,
            ..Self::default()
        }
    }
}

/// What one pass over the candidates observed.
#[derive(Debug, Default)]
pub struct RefreshReport {
    /// Entries whose stat fields were refreshed (content matches the blob).
    pub refreshed: Vec<PathBuf>,
    /// Entries whose content, mode or type changed: `<path>: needs update`.
    pub needs_update: Vec<PathBuf>,
    /// Worktree files that are missing: reported as `needs update` too, but
    /// tracked separately so `--ignore-missing` handling stays explicit.
    pub missing: Vec<PathBuf>,
    /// Unmerged paths: `<path>: needs merge` (reported even under `-q`).
    pub needs_merge: Vec<PathBuf>,
    /// Every reportable path in candidate (index) order, so `update-index
    /// --refresh` can render `needs update`/`needs merge` lines interleaved
    /// exactly like git's `refresh_index`. `add --refresh` ignores it.
    pub events: Vec<(PathBuf, RefreshEventKind)>,
    /// Whether the pass should exit 1 (git's refresh contract).
    pub has_errors: bool,
}

/// The git-rendered kind of a refresh report line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshEventKind {
    /// `<path>: needs update` (content, mode, type change or missing file).
    NeedsUpdate,
    /// `<path>: needs merge` (unmerged entry).
    NeedsMerge,
    /// A missing worktree file (renders as `needs update`, tracked separately
    /// for `--ignore-missing` documentation).
    Missing,
}

/// Silently refresh the stat fields of tracked stage-0 entries whose worktree
/// content still matches the staged blob — the stat side effect Git's staging
/// paths have and Libra's content-based status diff skips.
///
/// Git's `add <pathspec>` (`add_to_index`), `add -u`/`-A` and `commit -a`
/// (`add_files_to_cache`, which walks `diff-files`) re-record the stat data of
/// every matched tracked path whose stat changed but whose content did not.
/// Libra's staging loops only see content changes, so without this pass such
/// an entry stays stat-dirty and the stat-based plumbing (`diff-files`,
/// `diff-index`) keeps reporting it as `M` with an all-zero worktree OID right
/// after `add`/`commit -a` (issues/479 TC-0044, t3700#24; #660).
///
/// Boundary conditions:
/// - `pathspecs` limits the candidates; `None` means every tracked path
///   (`commit -a`).
/// - Paths in `exclude` (the ones the caller just staged or failed to stage)
///   are skipped: they already carry fresh stat data or must stay untouched.
/// - Intent-to-add placeholders are skipped; their content write belongs to
///   the staging loop (WT-07).
/// - Unmerged, skip-worktree, missing and content-changed entries are left
///   alone and never reported (`quiet` + `allow_unmerged` + `ignore_missing`).
/// - Like every `refresh_candidates` pass, each stage-0 candidate loses a stale
///   assume-unchanged (`CE_VALID`) bit (issues/637).
/// - Racily clean candidates are smudged (Git's `ce_smudge_racily_clean_entry`):
///   a candidate whose cached stat still matches the worktree but whose mtime
///   is not strictly older than `index_file_mtime` (the index file's mtime when
///   the caller loaded it; `None` trusts nothing) is content-checked, and when
///   the content differs from the staged blob — or cannot be read — its stat is
///   zeroed. Saving the index advances its mtime, which would otherwise turn
///   such an entry's racy uncertainty into trust and hide a same-size edit made
///   within the timestamp granularity.
///
/// Returns what changed; the caller saves the index when
/// [`UnchangedRefresh::index_changed`] is true.
pub fn refresh_unchanged_tracked(
    index: &mut Index,
    pathspecs: Option<&PathspecSet>,
    exclude: &[PathBuf],
    workdir: &Path,
    file_mode: bool,
    index_file_mtime: Option<SystemTime>,
) -> UnchangedRefresh {
    let exclude: std::collections::HashSet<&Path> = exclude.iter().map(PathBuf::as_path).collect();
    let candidates: Vec<PathBuf> = index
        .tracked_entries(0)
        .into_iter()
        .filter(|entry| !entry.flags.intent_to_add)
        .map(|entry| PathBuf::from(&entry.name))
        .filter(|path| !exclude.contains(path.as_path()))
        .filter(|path| pathspecs.is_none_or(|specs| specs.matches_path(path)))
        .collect();
    let cleared_assume_valid = candidates.iter().any(|path| {
        path.to_str()
            .and_then(|name| index.get(name, 0))
            .is_some_and(|entry| entry.flags.assume_valid)
    });
    let options = RefreshOptions {
        quiet: true,
        ignore_missing: true,
        allow_unmerged: true,
        file_mode,
    };
    let refreshed = refresh_candidates(index, &candidates, workdir, &options).refreshed;
    let refreshed_set: std::collections::HashSet<&Path> =
        refreshed.iter().map(PathBuf::as_path).collect();
    let smudged: Vec<PathBuf> = candidates
        .iter()
        .filter(|path| !refreshed_set.contains(path.as_path()))
        .filter(|path| smudge_if_racily_modified(index, path, workdir, index_file_mtime))
        .cloned()
        .collect();
    let index_changed = cleared_assume_valid || !refreshed.is_empty() || !smudged.is_empty();
    UnchangedRefresh {
        refreshed,
        smudged,
        index_changed,
    }
}

/// Outcome of [`refresh_unchanged_tracked`].
#[derive(Debug, Default)]
pub struct UnchangedRefresh {
    /// Entries whose stat was re-recorded because their content still matches.
    pub refreshed: Vec<PathBuf>,
    /// Racily clean entries whose content differs; their stat was zeroed.
    pub smudged: Vec<PathBuf>,
    /// Whether the in-memory index differs from the loaded one (refreshed or
    /// smudged entries, or a cleared stale `CE_VALID` bit).
    pub index_changed: bool,
}

/// Smudge one stage-0 entry whose cached stat matches the worktree but cannot
/// be trusted against `index_file_mtime`, when its content differs from the
/// staged blob or cannot be read. Returns whether the entry was smudged.
///
/// Unmerged, skip-worktree and missing paths, entries whose type or stat
/// already differs (those are left for the content-change paths), and entries
/// older than the index snapshot are never touched.
fn smudge_if_racily_modified(
    index: &mut Index,
    path: &Path,
    workdir: &Path,
    index_file_mtime: Option<SystemTime>,
) -> bool {
    let Some(name) = path.to_str() else {
        return false;
    };
    if [1u8, 2, 3]
        .iter()
        .any(|stage| index.get(name, *stage).is_some())
    {
        return false;
    }
    let Some((mode, hash, skip_worktree)) = index
        .get(name, 0)
        .map(|entry| (entry.mode, entry.hash, entry.flags.skip_worktree))
    else {
        return false;
    };
    if skip_worktree {
        return false;
    }
    let absolute = workdir.join(path);
    let Ok(metadata) = fs::symlink_metadata(&absolute) else {
        return false;
    };
    if !stat_matches(&metadata, index, name) {
        return false;
    }
    let trusted = match (metadata.modified(), index_file_mtime) {
        (Ok(mtime), Some(snapshot)) => mtime < snapshot,
        _ => false,
    };
    if trusted {
        return false;
    }
    if matches!(crate::command::calc_file_blob_hash(&absolute), Ok(actual) if actual == hash) {
        return false;
    }
    let mut smudged = IndexEntry::new(&metadata, hash, name.to_string());
    smudged.mode = mode;
    let zero = || git_internal::internal::index::Time::from_system_time(std::time::UNIX_EPOCH);
    smudged.ctime = zero();
    smudged.mtime = zero();
    smudged.dev = 0;
    smudged.ino = 0;
    smudged.uid = 0;
    smudged.gid = 0;
    crate::utils::index_ext::update_preserving_flags(index, smudged);
    true
}

/// Restrict `files` to entries that fall under at least one pathspec.
/// Extracted from `add` so `add --refresh` and `update-index --refresh` share
/// one candidate filter.
pub fn filter_refresh_candidates(files: &[PathBuf], pathspecs: &PathspecSet) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|file| pathspecs.matches_path(file.as_path()))
        .cloned()
        .collect()
}

/// Refresh the stat fields of every candidate whose working-tree content still
/// matches its staged blob, and classify the rest. The caller renders
/// `needs_update` / `missing` / `needs_merge` and decides the exit code.
///
/// Boundary conditions:
/// - Unmerged paths (any stage-1/2/3 entry) are reported once and never
///   refreshed; `--unmerged` skips them silently.
/// - Skip-worktree entries are skipped silently, as git's
///   `refresh_cache_ent` does.
/// - A missing worktree file is reported unless `ignore_missing` is set.
/// - Entry-type changes (file ↔ symlink) always count as needs-update; an
///   executable-bit change counts only under `file_mode` (git's trust-mode).
/// - The refreshed entry keeps its staged mode, hash and extended flags; only
///   the stat fields follow the worktree.
/// - Every stage-0 candidate loses a stale assume-unchanged (`CE_VALID`) bit,
///   whatever the outcome (issues/637). Libra has no assume-unchanged
///   semantics, so the bit can only be a leftover from git-internal <= 0.10.3;
///   Git would keep it here and keep skipping the worktree stat.
pub fn refresh_candidates(
    index: &mut Index,
    candidates: &[PathBuf],
    workdir: &Path,
    options: &RefreshOptions,
) -> RefreshReport {
    let mut report = RefreshReport::default();
    for candidate in candidates {
        let Some(name) = candidate.to_str() else {
            continue;
        };
        // Unmerged: stages 1-3 exist. Report once, never refresh (git's
        // refresh_index skips the whole path's entries after the stage-1
        // report).
        if [1u8, 2, 3]
            .iter()
            .any(|stage| index.get(name, *stage).is_some())
        {
            if options.allow_unmerged {
                continue;
            }
            report.needs_merge.push(candidate.clone());
            report
                .events
                .push((candidate.clone(), RefreshEventKind::NeedsMerge));
            report.has_errors = true;
            continue;
        }
        // Snapshot the stage-0 fields; `IndexEntry` is not `Clone`, and the
        // refresh decision must not hold the index borrow while hashing.
        let Some((mode, hash, skip_worktree)) = index
            .get(name, 0)
            .map(|entry| (entry.mode, entry.hash, entry.flags.skip_worktree))
        else {
            continue;
        };
        crate::utils::index_ext::clear_assume_valid(index, name, 0);
        if skip_worktree {
            // CE_SKIP_WORKTREE: the user promised the worktree copy is good.
            continue;
        }

        let absolute = workdir.join(candidate);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(_) => {
                if options.ignore_missing {
                    continue;
                }
                if !options.quiet {
                    report.missing.push(candidate.clone());
                    report
                        .events
                        .push((candidate.clone(), RefreshEventKind::Missing));
                    report.has_errors = true;
                }
                continue;
            }
        };

        // Entry type: regular (0o100000) vs symlink (0o120000) — a change is
        // always a needs-update, independent of `core.fileMode` (FM-03).
        let disk_type = if metadata.file_type().is_symlink() {
            0o120000
        } else {
            0o100000
        };
        if disk_type != mode & 0o170000 {
            if !options.quiet {
                report.needs_update.push(candidate.clone());
                report
                    .events
                    .push((candidate.clone(), RefreshEventKind::NeedsUpdate));
                report.has_errors = true;
            }
            continue;
        }
        // Executable bit (regular files only; trust mode mirrors core.fileMode).
        if options.file_mode && disk_type == 0o100000 {
            let disk_exec = is_executable(&metadata);
            if disk_exec != (mode & 0o111 != 0) {
                if !options.quiet {
                    report.needs_update.push(candidate.clone());
                    report
                        .events
                        .push((candidate.clone(), RefreshEventKind::NeedsUpdate));
                    report.has_errors = true;
                }
                continue;
            }
        }

        // Stat comparison: ctime + mtime + size. A stat-equal entry is trusted
        // (git's racy-clean heuristic — content changed within the same
        // timestamp granularity is invisible to refresh, exactly as in git).
        if stat_matches(&metadata, index, name) {
            continue;
        }
        // Stat changed: re-hash with the REAL blob hash (LFS/symlink aware).
        // Only a matching hash refreshes the stat; a differing one reports
        // needs-update without touching the entry.
        match crate::command::calc_file_blob_hash(&absolute) {
            Ok(actual) if actual == hash => {
                // Refresh: keep mode/hash/flags, take the fresh stat fields.
                let mut refreshed = IndexEntry::new(&metadata, hash, name.to_string());
                refreshed.mode = mode;
                crate::utils::index_ext::update_preserving_flags(index, refreshed);
                report.refreshed.push(candidate.clone());
            }
            _ => {
                if !options.quiet {
                    report.needs_update.push(candidate.clone());
                    report
                        .events
                        .push((candidate.clone(), RefreshEventKind::NeedsUpdate));
                    report.has_errors = true;
                }
            }
        }
    }
    report
}

/// Whether the worktree stat matches the stage-0 entry's stat fields.
///
/// Compares ctime + mtime + size against the recorded fields. Entries built by
/// `new_from_blob` (cacheinfo / index-info) carry zeroed times, so they are
/// always considered stale until a refresh records real stat data — the
/// intended plumbing behavior.
fn stat_matches(metadata: &fs::Metadata, index: &Index, name: &str) -> bool {
    let Some(entry) = index.get(name, 0) else {
        return false;
    };
    // The index stores the low 32 bits of the size (git-internal truncates
    // like Git's `sd_size`), so compare the same truncated value Git does
    // (`sd_size != (unsigned int) st_size`); a full-width comparison would
    // make every clean file of 4 GiB or more look stat-dirty and be re-hashed
    // on each refresh (issues/479 FX-479-03 review R3-P1-1).
    if metadata.len() as u32 != entry.size {
        return false;
    }
    let (disk_ctime, disk_mtime) = stat_times(metadata);
    let entry_ctime = git_internal::internal::index::Time::from_system_time(disk_ctime);
    let entry_mtime = git_internal::internal::index::Time::from_system_time(disk_mtime);
    entry.ctime == entry_ctime && entry.mtime == entry_mtime
}

/// The (ctime, mtime) pair of a metadata handle, following git-internal's
/// `index_ctime`/`index_mtime` mapping on every platform.
fn stat_times(metadata: &fs::Metadata) -> (std::time::SystemTime, std::time::SystemTime) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::new(
                metadata.ctime().max(0) as u64,
                metadata.ctime_nsec().max(0) as u32,
            );
        let mtime = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::new(
                metadata.mtime().max(0) as u64,
                metadata.mtime_nsec().max(0) as u32,
            );
        (ctime, mtime)
    }
    #[cfg(not(unix))]
    {
        let created = metadata
            .created()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let modified = metadata.modified().unwrap_or(created);
        (created, modified)
    }
}

/// Whether a worktree file carries the executable bit (Unix only; elsewhere
/// regular files are never executable).
fn is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Force the executable bit on a tracked stage-0 entry — the shared core of
/// `add --chmod` and `update-index --chmod`.
///
/// Returns `Err(message)` when the path has no stage-0 entry or the entry is
/// not a regular file (symlinks `120000` and gitlinks `160000` carry no
/// executable bit); callers choose how to surface that (`add` records a
/// rejection, `update-index` dies with `cannot chmod <flip> '<path>'`).
/// Returns `Ok(true)` when the mode flipped, `Ok(false)` when it already
/// matched (a no-op). The entry's blob, size and extended flags are kept; the
/// stat fields are zeroed so the next status content-compares this entry
/// (2026-08-06 R0-8 review).
pub fn chmod_entry(index: &mut Index, path: &str, target_mode: u32) -> Result<bool, String> {
    let Some(existing) = index.get(path, 0) else {
        return Err(format!("cannot chmod '{path}': not in the index"));
    };
    let (mode, hash, size) = (existing.mode, existing.hash, existing.size);
    if mode & 0o170000 != 0o100000 {
        return Err(format!(
            "cannot chmod '{path}': entry is not a regular file (mode {mode:o})"
        ));
    }
    if mode == target_mode {
        return Ok(false);
    }
    let mut updated = IndexEntry::new_from_blob(path.to_string(), hash, size);
    updated.mode = target_mode;
    crate::utils::index_ext::update_preserving_flags(index, updated);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use git_internal::internal::{
        index::{Index, IndexEntry, Time},
        object::blob::Blob,
    };

    use super::refresh_unchanged_tracked;

    /// Track `name` with `staged` as its blob while the worktree holds
    /// `worktree` and the entry keeps the worktree file's exact stat.
    fn racy_index(dir: &std::path::Path, name: &str, staged: &str, worktree: &str) -> Index {
        let file = dir.join(name);
        std::fs::write(&file, worktree).expect("write worktree file");
        let metadata = std::fs::symlink_metadata(&file).expect("stat");
        let mut index = Index::new();
        index.add(IndexEntry::new(
            &metadata,
            Blob::from_content(staged).id,
            name.to_string(),
        ));
        index
    }

    fn mtime(dir: &std::path::Path, name: &str) -> std::time::SystemTime {
        std::fs::symlink_metadata(dir.join(name))
            .and_then(|meta| meta.modified())
            .expect("mtime")
    }

    /// Git's `ce_smudge_racily_clean_entry`: a stat-matching entry that is not
    /// older than the index snapshot and whose content differs is smudged.
    #[test]
    fn racy_entry_with_different_content_is_smudged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut index = racy_index(dir.path(), "racy.txt", "old\n", "new\n");
        let snapshot = mtime(dir.path(), "racy.txt") - Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], dir.path(), true, Some(snapshot));
        assert_eq!(outcome.smudged, vec![std::path::PathBuf::from("racy.txt")]);
        assert!(outcome.refreshed.is_empty());
        assert!(outcome.index_changed);
        let zero = Time::from_system_time(std::time::UNIX_EPOCH);
        let entry = index.get("racy.txt", 0).expect("still tracked");
        assert!(entry.mtime == zero && entry.ctime == zero, "stat zeroed");
        assert_eq!(entry.hash, Blob::from_content("old\n").id, "blob untouched");
    }

    /// A racily clean entry whose content still matches stays as it is.
    #[test]
    fn racy_entry_with_same_content_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut index = racy_index(dir.path(), "same.txt", "same\n", "same\n");
        let snapshot = mtime(dir.path(), "same.txt") - Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], dir.path(), true, Some(snapshot));
        assert!(outcome.smudged.is_empty() && !outcome.index_changed);
    }

    /// An entry strictly older than the index snapshot is trusted and never
    /// re-read, matching the `status` racily-clean guard.
    #[test]
    fn entry_older_than_snapshot_is_trusted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut index = racy_index(dir.path(), "old.txt", "old\n", "new\n");
        let snapshot = mtime(dir.path(), "old.txt") + Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], dir.path(), true, Some(snapshot));
        assert!(outcome.smudged.is_empty() && !outcome.index_changed);
    }

    /// R2-P1-1: a racy candidate that cannot be read is smudged (content
    /// unknown) instead of failing or being trusted. The permission change
    /// happens before the entry records its stat, because `chmod` moves ctime.
    #[cfg(unix)]
    #[test]
    fn unreadable_racy_entry_is_smudged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("locked.txt");
        std::fs::write(&file, "same\n").expect("write");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
            .expect("make unreadable");
        let readable = std::fs::read(&file).is_ok();
        let metadata = std::fs::symlink_metadata(&file).expect("stat");
        let mut index = Index::new();
        index.add(IndexEntry::new(
            &metadata,
            Blob::from_content("same\n").id,
            "locked.txt".to_string(),
        ));
        let snapshot = metadata.modified().expect("mtime") - Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], dir.path(), true, Some(snapshot));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .expect("restore mode");
        if readable {
            // Running as root: permissions do not block the read.
            return;
        }
        assert_eq!(
            outcome.smudged,
            vec![std::path::PathBuf::from("locked.txt")]
        );
        assert!(outcome.index_changed);
    }

    /// R3-P1-1: a clean file of 4 GiB or more matches its entry's truncated
    /// size, so refresh trusts it instead of re-hashing the whole payload.
    #[test]
    fn clean_oversized_file_matches_truncated_index_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("huge.bin");
        // Sparse on APFS/ext4: no real 5 GiB is written.
        std::fs::File::create(&file)
            .and_then(|handle| handle.set_len(5 << 30))
            .expect("sparse file");
        let metadata = std::fs::symlink_metadata(&file).expect("stat");
        let mut index = Index::new();
        index.add(IndexEntry::new(
            &metadata,
            Blob::from_content("placeholder").id,
            "huge.bin".to_string(),
        ));
        assert!(super::stat_matches(&metadata, &index, "huge.bin"));
        // Trusted against a later snapshot: nothing is hashed or rewritten.
        let snapshot = metadata.modified().expect("mtime") + Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], dir.path(), true, Some(snapshot));
        assert!(outcome.refreshed.is_empty() && !outcome.index_changed);
    }

    /// R2-P1-1: an unreadable racily clean LFS-tracked file reaches the
    /// LFS branch of `calc_file_blob_hash`, which must return the read error
    /// (the panicking `generate_pointer_file` would abort here) so the entry
    /// is smudged as content-unknown.
    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn unreadable_racy_lfs_entry_is_smudged_without_panic() {
        use std::os::unix::fs::PermissionsExt;

        use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

        // SAFETY: `geteuid` only reads the process credential and has no pointer argument.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped (permission bits do not constrain root)");
            return;
        }
        let repo = tempfile::tempdir().expect("tempdir");
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        // The canonical worktree root, as production callers pass it.
        let workdir = crate::utils::util::working_dir();
        std::fs::write(workdir.join(".gitattributes"), "*.bin filter=lfs\n").expect("attributes");
        let file = workdir.join("big.bin");
        std::fs::write(&file, "payload\n").expect("write");
        assert!(
            crate::utils::lfs::is_lfs_tracked(&file),
            "fixture must take the LFS branch"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
            .expect("make unreadable");
        let metadata = std::fs::symlink_metadata(&file).expect("stat");
        let hash_result = crate::command::calc_file_blob_hash(&file);
        let mut index = Index::new();
        index.add(IndexEntry::new(
            &metadata,
            Blob::from_content("payload\n").id,
            "big.bin".to_string(),
        ));
        let snapshot = metadata.modified().expect("mtime") - Duration::from_secs(2);
        let outcome =
            refresh_unchanged_tracked(&mut index, None, &[], &workdir, true, Some(snapshot));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .expect("restore mode");
        assert!(hash_result.is_err(), "the LFS read failure is an Err");
        assert_eq!(outcome.smudged, vec![std::path::PathBuf::from("big.bin")]);
    }
}
