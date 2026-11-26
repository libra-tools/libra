//! Repository-scoped, quota-reserved scratch space for commit previews.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tempfile::TempDir;

use crate::utils::preview_object::MAX_CACHE_BYTES;

const MAX_REPOSITORY_BYTES: u64 = 4 * MAX_CACHE_BYTES;
const MAX_SCANNED_ENTRIES: usize = 512;
const MAX_SCANNED_RUNS: usize = 256;
const MAX_STALE_REMOVALS: usize = 32;
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug)]
pub struct PreviewScratch {
    run_lock: File,
    directory: TempDir,
}

impl PreviewScratch {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

impl Drop for PreviewScratch {
    fn drop(&mut self) {
        let _ = self.run_lock.unlock();
    }
}

/// Reserve one bounded preview run under `.libra/tmp/commit-preview`.
pub fn create(libra_dir: &Path) -> io::Result<PreviewScratch> {
    create_with_limits(
        libra_dir,
        MAX_CACHE_BYTES,
        MAX_REPOSITORY_BYTES,
        SystemTime::now(),
    )
}

fn create_with_limits(
    libra_dir: &Path,
    reservation_bytes: u64,
    repository_bytes: u64,
    now: SystemTime,
) -> io::Result<PreviewScratch> {
    let namespace = libra_dir.join("tmp/commit-preview");
    fs::create_dir_all(&namespace)?;
    let namespace_lock = open_lock(&namespace.join(".lock"))?;
    namespace_lock.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "another commit preview is reserving scratch space; retry shortly",
        ),
        fs::TryLockError::Error(error) => error,
    })?;

    let reserved = scan_and_prune(&namespace, now)?;
    if reserved.saturating_add(reservation_bytes) > repository_bytes {
        return Err(io::Error::other(format!(
            "commit preview scratch quota of {repository_bytes} bytes is exhausted; wait for another preview to finish or remove stale directories under '{}'",
            namespace.display()
        )));
    }

    let directory = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&namespace)?;
    let reservation = directory.path().join("reservation");
    let reservation_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&reservation)?;
    reservation_file.set_len(reservation_bytes)?;
    let run_lock = open_lock(&directory.path().join("run.lock"))?;
    run_lock.try_lock().map_err(io::Error::from)?;
    fs::create_dir(directory.path().join("objects"))?;
    Ok(PreviewScratch {
        run_lock,
        directory,
    })
}

fn scan_and_prune(namespace: &Path, now: SystemTime) -> io::Result<u64> {
    let mut reserved = 0u64;
    let mut scanned = 0usize;
    let mut runs = 0usize;
    let mut removed = 0usize;
    for entry in fs::read_dir(namespace)? {
        let entry = entry?;
        scanned += 1;
        if scanned > MAX_SCANNED_ENTRIES {
            return Err(io::Error::other(format!(
                "commit preview scratch contains more than {MAX_SCANNED_ENTRIES} entries; remove stale data under '{}'",
                namespace.display()
            )));
        }
        if !entry.file_type()?.is_dir() || !entry.file_name().to_string_lossy().starts_with("run-")
        {
            continue;
        }
        runs += 1;
        if runs > MAX_SCANNED_RUNS {
            return Err(io::Error::other(format!(
                "commit preview scratch contains more than {MAX_SCANNED_RUNS} runs; remove stale directories under '{}'",
                namespace.display()
            )));
        }
        let path = entry.path();
        let reservation = path.join("reservation");
        let metadata = fs::metadata(&reservation).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to inspect commit preview reservation '{}': {error}; remove the malformed run directory",
                    reservation.display()
                ),
            )
        })?;
        let size = metadata.len();
        let modified = metadata.modified().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to read commit preview reservation timestamp '{}': {error}; remove the malformed run directory",
                    reservation.display()
                ),
            )
        })?;
        let stale = now.duration_since(modified).unwrap_or_default() >= STALE_AFTER;
        if stale && removed < MAX_STALE_REMOVALS && run_is_inactive(&path)? {
            fs::remove_dir_all(&path)?;
            removed += 1;
        } else {
            reserved = reserved.saturating_add(size);
        }
    }
    Ok(reserved)
}

fn run_is_inactive(path: &Path) -> io::Result<bool> {
    let lock = open_lock(&path.join("run.lock"))?;
    match lock.try_lock() {
        Ok(()) => {
            lock.unlock()?;
            Ok(true)
        }
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn open_lock(path: &PathBuf) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;

    use super::*;

    #[test]
    fn reservations_enforce_repo_quota_and_release_on_drop() {
        let root = tempfile::tempdir().expect("create scratch root");
        let first =
            create_with_limits(root.path(), 8, 16, SystemTime::now()).expect("reserve first run");
        let second =
            create_with_limits(root.path(), 8, 16, SystemTime::now()).expect("reserve second run");
        let error = create_with_limits(root.path(), 8, 16, SystemTime::now())
            .expect_err("third reservation must exceed quota");
        assert!(error.to_string().contains("quota"), "{error}");
        drop(first);
        drop(second);
        create_with_limits(root.path(), 8, 16, SystemTime::now())
            .expect("released runs free the quota");
    }

    #[test]
    fn stale_inactive_run_is_scavenged_with_bounded_scan() {
        let root = tempfile::tempdir().expect("create scratch root");
        let namespace = root.path().join("tmp/commit-preview");
        let stale = namespace.join("run-stale");
        fs::create_dir_all(&stale).expect("create stale run");
        let reservation = stale.join("reservation");
        fs::write(&reservation, b"").expect("create reservation");
        fs::File::open(&reservation)
            .expect("open reservation")
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60)),
            )
            .expect("age reservation");
        open_lock(&stale.join("run.lock")).expect("create stale run lock");

        create_with_limits(root.path(), 8, 16, SystemTime::now())
            .expect("stale run should be reclaimed");
        assert!(!stale.exists(), "stale inactive run must be removed");
    }

    #[test]
    fn active_run_without_reservation_fails_closed() {
        let root = tempfile::tempdir().expect("create scratch root");
        let namespace = root.path().join("tmp/commit-preview");
        let active = namespace.join("run-active");
        fs::create_dir_all(&active).expect("create active run");
        let lock = open_lock(&active.join("run.lock")).expect("create active run lock");
        lock.try_lock().expect("hold active run lock");

        let error = create_with_limits(root.path(), 8, 16, SystemTime::now())
            .expect_err("malformed active run must not bypass the repository quota");
        assert!(error.to_string().contains("reservation"), "{error}");
        assert_eq!(
            fs::read_dir(&namespace)
                .expect("read scratch namespace")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("run-"))
                .count(),
            1,
            "failed reservation must not create another run"
        );
    }
}
