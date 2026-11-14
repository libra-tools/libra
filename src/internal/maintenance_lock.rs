//! Repository maintenance exclusion lock (plan-20260714 Part C §C.4.3
//! writer-vs-deleter).
//!
//! Deleting an object is only safe while nothing can publish a reference to
//! it. The two-scan quarantine ledger in `gc` proves an object was
//! unreachable at two separated points in time; it cannot prove that nothing
//! referenced it in the window between the last scan and the `unlink`. Nor
//! can a database transaction: a worktree's index, the merge/rebase
//! sidecars and the agent-run manifests are FILES, so a stage of content
//! that happens to hash to an already-quarantined object commits without
//! touching SQLite at all.
//!
//! This is that missing exclusion, and it is deliberately the crudest thing
//! that works — one advisory lock file, `\<storage\>/maintenance.lock`:
//!
//! - every command that can publish an object reference holds it SHARED for
//!   the whole command (`crate::cli` takes it once, at dispatch, from the
//!   scope inventory — so a new command cannot forget to);
//! - every phase that physically deletes an object holds it EXCLUSIVE across
//!   the whole "decide what is unreachable → delete it" sequence.
//!
//! Two publishers never block each other, so the cost on the normal path is
//! one `flock` per command. A deleter and a publisher can never overlap, so
//! the ordering is forced: either the publish is visible to the deleter's
//! scan (object retained), or the publisher starts after the deletion and
//! re-creates what it needs — never the interleaving where a reference
//! appears for bytes already gone.
//!
//! Waiting is BOUNDED. A deleter that cannot get exclusive access reports
//! that fact; it never blocks behind a long-lived `libra code` session
//! forever, and it never proceeds without the exclusion (deletion is
//! deferred, which costs disk, not data).

use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// Which way this process currently holds the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Shared,
    Exclusive,
}

/// One repository's hold, tracked per process because `flock` is per
/// open-file-description: a second `File` opened by the SAME process
/// conflicts with the first exactly as a different process would. Without
/// this, a deletion phase that legitimately re-enters the lock (an
/// obliteration holding it across the tombstone write and then reaching
/// `delete_payload`, which re-checks) would block on itself for the full
/// timeout and then report a conflict that does not exist.
#[derive(Debug)]
struct RepoState {
    mode: Mode,
    depth: usize,
    /// The pathname this hold was taken through. Kept so a LATER acquisition
    /// through the same pathname can notice that the file behind it has been
    /// replaced — at which point the two holders would be flocking different
    /// inodes and the exclusion would silently stop existing.
    path: PathBuf,
    /// Held for its `Drop`: closing the descriptor releases the advisory lock.
    _file: File,
}

/// Identity of a lock file. `flock` is per INODE, so this is what the
/// kernel actually serialises on — and therefore what re-entrancy has to be
/// keyed by.
///
/// A pathname is not that identity. Two aliases of one file (a bind mount, a
/// symlinked worktree) canonicalize differently while sharing an inode, and
/// keying by path would then treat one inode's exclusive hold as two
/// independent claims — the second acquisition blocking on the first, inside
/// the very mutex that has to be released for the first to finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(windows, allow(dead_code))]
struct LockIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    /// Fallback identity on platforms without a stable inode: the canonical
    /// path. Conservative — at worst two spellings are treated as different
    /// repositories, so both take the real `flock`, which is still correct.
    #[cfg(not(unix))]
    path: (),
}

/// Keyed per repository, not global. A process that holds repository A's lock
/// has said nothing about repository B — treating the hold as process-wide
/// would let it publish into B, re-entrantly and without ever opening B's
/// lock file, while another process deleted B's objects. One process
/// legitimately touches several repositories: alternates, a task worktree, an
/// agent working on a clone.
static LOCKS: LazyLock<Mutex<std::collections::HashMap<LockKey, RepoState>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Inode identity where the platform provides one, canonical path elsewhere.
#[cfg(unix)]
type LockKey = LockIdentity;
#[cfg(not(unix))]
type LockKey = PathBuf;

/// Duplicate a [`LockKey`] for map insertion while the original is still
/// needed. Platform-split so neither side trips clippy: the Unix key is a
/// `Copy` dev/ino identity (`clone_on_copy`), the Windows key is a `PathBuf`
/// (a real clone).
#[cfg(unix)]
fn dup_lock_key(key: &LockKey) -> LockKey {
    *key
}
#[cfg(not(unix))]
fn dup_lock_key(key: &LockKey) -> LockKey {
    key.clone()
}

fn locks() -> std::sync::MutexGuard<'static, std::collections::HashMap<LockKey, RepoState>> {
    LOCKS.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The identity of the OPENED lock file, plus proof that the path still names
/// it.
///
/// The second half matters: if the lock file is replaced between one holder's
/// open and the next, each ends up flocking a different inode and the
/// exclusion silently stops existing — a publisher and a deleter would both
/// believe they hold the repository. Rather than serialise on a file nobody
/// else can see, this refuses.
fn lock_key(path: &Path, file: &File) -> CliResult<LockKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file.metadata().map_err(|error| {
            CliError::fatal(format!(
                "failed to identify the repository maintenance lock '{}': {error}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        let named = std::fs::metadata(path).map_err(|error| {
            CliError::fatal(format!(
                "the repository maintenance lock '{}' disappeared while it was being taken: \
                 {error}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
            return Err(CliError::fatal(format!(
                "the repository maintenance lock '{}' was replaced while it was being taken, so \
                 it can no longer serialise publishers against deletions",
                path.display()
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(
                "something outside Libra is rewriting this file; remove the replacement and \
                 retry with no other Libra command running",
            ));
        }
        Ok(LockIdentity {
            device: opened.dev(),
            inode: opened.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    }
}

/// How long a deletion phase waits for publishers to finish before giving up.
///
/// Long enough that ordinary short commands (an `add`, a `commit`) are simply
/// waited out; short enough that a maintenance run behind an interactive
/// session reports the deferral instead of appearing to hang.
pub const DELETION_LOCK_WAIT: Duration = Duration::from_secs(10);

/// Poll interval while waiting for the exclusive lock.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn lock_path(storage: &Path) -> PathBuf {
    storage.join("maintenance.lock")
}

fn open_lock_file(path: &Path) -> CliResult<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::fatal(format!(
                "failed to create the maintenance lock directory '{}': {error}",
                parent.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to open the repository maintenance lock '{}': {error}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    if let Ok(Some(shared)) = crate::utils::shared_perm::get_shared_mode_for_path(path) {
        let _ = crate::utils::shared_perm::adjust_shared_perm(path, &shared);
    }
    Ok(file)
}

/// Refuse when the pathname we just opened resolves to a DIFFERENT file than
/// a live hold taken through that same pathname.
///
/// The per-acquisition check in [`lock_key`] catches a replacement between
/// our own open and our own lock. It cannot catch this one: holder A locks
/// inode X, the file is replaced, holder B opens inode Y — and B's own
/// path-vs-descriptor comparison agrees, because by then the path really does
/// name Y. Both would then believe they hold the repository while flocking
/// different inodes, which is the exclusion silently ceasing to exist.
///
/// Scope, stated plainly: this covers holds this PROCESS is tracking. An
/// external actor replacing `.libra/maintenance.lock` between two separate
/// processes defeats advisory locking in a way no advisory scheme can
/// detect — the same is true of `flock` in git, and the answer there and here
/// is that nothing outside the tool may rewrite the lock file.
fn ensure_not_replaced_under_a_live_hold(
    locks: &std::collections::HashMap<LockKey, RepoState>,
    path: &Path,
    key: &LockKey,
) -> CliResult<()> {
    let replaced = locks
        .iter()
        .any(|(held_key, state)| state.path == path && held_key != key);
    if !replaced {
        return Ok(());
    }
    Err(CliError::fatal(format!(
        "the repository maintenance lock '{}' was replaced while this process still holds the \
         previous one, so it can no longer serialise publishers against deletions",
        path.display()
    ))
    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
    .with_hint(
        "something outside Libra is rewriting this file; remove the replacement and retry with \
         no other Libra command running",
    ))
}

/// A held maintenance lock. Released when dropped (including on unwind).
///
/// Nested acquisitions within one process are reference-counted, so passing
/// the guard down a call chain is never required for correctness — the inner
/// acquisition sees the outer one and the release happens once, when the
/// outermost guard drops.
#[derive(Debug)]
pub struct MaintenanceLock {
    // A guard is a claim on ONE repository's hold, not on a descriptor:
    // `Drop` decrements that repository's count, and the descriptor is closed
    // (releasing the advisory lock) only when its last guard goes.
    key: LockKey,
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        let mut locks = locks();
        let Some(state) = locks.get_mut(&self.key) else {
            return;
        };
        state.depth = state.depth.saturating_sub(1);
        if state.depth == 0 {
            locks.remove(&self.key);
        }
    }
}

impl MaintenanceLock {
    /// Take the lock SHARED, for a command that may publish object
    /// references. Blocks only while a deletion phase is running, which is
    /// bounded by [`DELETION_LOCK_WAIT`] on the deleter's side.
    pub fn shared(storage: &Path) -> CliResult<Self> {
        let path = lock_path(storage);
        let file = open_lock_file(&path)?;
        let key = lock_key(&path, &file)?;

        // The process-wide table is consulted, then RELEASED. Nothing may
        // block while holding it: `flock` here can wait for another process
        // for as long as that process likes, and a thread parked inside the
        // mutex freezes every other repository's bookkeeping too — which is a
        // whole-process deadlock, not a slow path.
        {
            let locks = locks();
            ensure_not_replaced_under_a_live_hold(&locks, &path, &key)?;
        }
        if let Some(existing) = Self::join_existing(&key) {
            // Already held for THIS repository — shared or exclusive, either
            // covers a publisher.
            return Ok(existing);
        }

        // TEST-ONLY contention marker, emitted from INSIDE the acquisition so
        // it cannot outlive it: a regression that wants to prove a publisher
        // waited for the lock has no other way to distinguish "blocked" from
        // "descheduled". Debug builds only, opt-in by `LIBRA_TEST=1`, written
        // to fixed paths beside the lock, created no-follow so a planted
        // symlink cannot redirect the write.
        //
        // The probe RETAINS a successful lock rather than taking one twice:
        // re-locking a handle that already holds a lock is unspecified, so a
        // debug publisher must not do it. Only `WouldBlock` falls through to
        // the blocking acquisition.
        #[cfg(not(debug_assertions))]
        let already_held = false;
        #[cfg(debug_assertions)]
        let mut already_held = false;
        #[cfg(debug_assertions)]
        if std::env::var("LIBRA_TEST").as_deref() == Ok("1") {
            match file.try_lock_shared() {
                Ok(()) => already_held = true,
                Err(std::fs::TryLockError::WouldBlock) => {
                    // Two signals. The `.attempted` one is what a test polls
                    // when it has deliberately blocked the first with a
                    // symlink — without it, "the sentinel is untouched" could
                    // just mean the child never got here.
                    let _ = File::options()
                        .create_new(true)
                        .write(true)
                        .open(storage.join("publication-barrier.attempted"));
                    let _ = File::options()
                        .create_new(true)
                        .write(true)
                        .open(storage.join("publication-barrier"));
                }
                Err(std::fs::TryLockError::Error(_)) => {}
            }
        }
        if !already_held {
            file.lock_shared().map_err(|error| {
                CliError::fatal(format!(
                    "failed to take the repository maintenance lock '{}' for reading: {error}",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        }

        // Another thread of this process may have recorded a hold while we
        // were waiting. Join it and let our own descriptor close: `flock` is
        // per open-file-description, so dropping ours leaves theirs standing.
        let mut locks = locks();
        if let Some(state) = locks.get_mut(&key) {
            state.depth += 1;
            return Ok(Self { key });
        }
        locks.insert(
            dup_lock_key(&key),
            RepoState {
                mode: Mode::Shared,
                depth: 1,
                path,
                _file: file,
            },
        );
        Ok(Self { key })
    }

    /// Acquire a publication hold without blocking the executor on contention.
    /// `None` means the original acquisition deadline elapsed. A successful
    /// guard still has the existing RAII lifetime, not an expiring lease.
    /// File open/metadata use the existing local filesystem contract.
    /// Requires a Tokio runtime with timers enabled.
    #[cfg_attr(
        not(all(test, feature = "test-live-agent")),
        allow(
            dead_code,
            reason = "CX32 qualifies this opt-in API before its CX09 consumer"
        )
    )]
    pub(crate) async fn shared_until(storage: &Path, deadline: Instant) -> CliResult<Option<Self>> {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let path = lock_path(storage);
        let file = open_lock_file(&path)?;
        let key = lock_key(&path, &file)?;
        loop {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            // The named file may have changed during the preceding sleep.
            // Recheck before joining a process hold, not only after an OS try.
            if lock_key(&path, &file)? != key {
                return Err(CliError::fatal(format!(
                    "the repository maintenance lock '{}' changed while it was being acquired",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("retry with no other Libra command running; if this persists, inspect what is replacing the lock file"));
            }
            {
                let held = locks();
                ensure_not_replaced_under_a_live_hold(&held, &path, &key)?;
            }
            // A different task may have acquired a compatible process hold
            // since our previous poll. Exclusive holds retain the original
            // process-reentry semantics; this is not task-level exclusion.
            if let Some(guard) = Self::join_existing(&key) {
                return Ok((Instant::now() < deadline).then_some(guard));
            }
            match file.try_lock_shared() {
                Ok(()) => {
                    if lock_key(&path, &file)? != key {
                        return Err(CliError::fatal(format!(
                            "the repository maintenance lock '{}' changed while it was being acquired",
                            path.display()
                        ))
                        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                        .with_hint("retry with no other Libra command running; if this persists, inspect what is replacing the lock file"));
                    }
                    // Register-or-join is one short atomic map operation.
                    // Keep filesystem work and every await outside this guard.
                    let mut held = locks();
                    ensure_not_replaced_under_a_live_hold(&held, &path, &key)?;
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    if let Some(state) = held.get_mut(&key) {
                        state.depth += 1;
                        // `file` is the losing fd; dropping it cannot release
                        // the descriptor retained by this existing state.
                    } else {
                        held.insert(
                            dup_lock_key(&key),
                            RepoState {
                                mode: Mode::Shared,
                                depth: 1,
                                path,
                                _file: file,
                            },
                        );
                    }
                    drop(held);
                    let guard = Self { key };
                    return Ok((Instant::now() < deadline).then_some(guard));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(None);
                    }
                    tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(CliError::fatal(format!(
                        "failed to take the repository maintenance lock '{}' for reading: {error}",
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::IoWriteFailed));
                }
            }
        }
    }

    /// Join a hold this process already has, if any.
    fn join_existing(key: &LockKey) -> Option<Self> {
        let mut locks = locks();
        let state = locks.get_mut(key)?;
        state.depth += 1;
        Some(Self {
            key: dup_lock_key(key),
        })
    }

    /// Take the lock EXCLUSIVE for a deletion phase, waiting at most `wait`.
    ///
    /// Returns `Ok(None)` when publishers still hold it at the deadline —
    /// the caller decides whether that is a deferral (`gc` keeps the objects)
    /// or a refusal (`obliterate` must not silently skip an erase).
    pub fn try_exclusive(storage: &Path, wait: Duration) -> CliResult<Option<Self>> {
        let path = lock_path(storage);
        let file = open_lock_file(&path)?;
        let key = lock_key(&path, &file)?;

        // Consulted and RELEASED before any waiting — see `shared`. The poll
        // loop below sleeps for up to `wait`, and doing that inside the
        // process-wide table would stall every other repository's bookkeeping
        // for the same duration.
        {
            let mut locks = locks();
            ensure_not_replaced_under_a_live_hold(&locks, &path, &key)?;
            match locks.get_mut(&key).map(|state| state.mode) {
                // Re-entrant: this process already excludes everyone else
                // FROM THIS repository.
                Some(Mode::Exclusive) => {
                    if let Some(state) = locks.get_mut(&key) {
                        state.depth += 1;
                    }
                    return Ok(Some(Self { key }));
                }
                // A shared hold cannot be UPGRADED — another process may hold
                // it shared too, and waiting would deadlock against
                // ourselves. Report contention immediately.
                Some(Mode::Shared) => return Ok(None),
                None => {}
            }
        }

        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    let mut locks = locks();
                    // A concurrent thread of this process cannot have
                    // recorded a hold — it would have had to take the same
                    // `flock` we just took — but if it somehow did, defer
                    // rather than claim an exclusivity we do not have.
                    if locks.contains_key(&key) {
                        return Ok(None);
                    }
                    locks.insert(
                        dup_lock_key(&key),
                        RepoState {
                            mode: Mode::Exclusive,
                            depth: 1,
                            path,
                            _file: file,
                        },
                    );
                    return Ok(Some(Self { key }));
                }
                // Contended: a publisher (or another deleter) holds it.
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                // A genuine I/O failure is never retried: retrying it would
                // turn a broken lock file into a silent "deletion deferred
                // forever" instead of a diagnosable error.
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(CliError::fatal(format!(
                        "failed to take the repository maintenance lock '{}' for deletion: \
                         {error}",
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::IoWriteFailed));
                }
            }
        }
    }

    /// [`Self::try_exclusive`], but a contended lock is a hard refusal.
    ///
    /// For deletions that must not be silently skipped — an obliteration is
    /// an erasure the user asked for, and reporting success without
    /// performing it is the one outcome worse than failing.
    pub fn exclusive_or_refuse(storage: &Path, action: &str) -> CliResult<Self> {
        match Self::try_exclusive(storage, DELETION_LOCK_WAIT)? {
            Some(lock) => Ok(lock),
            None => Err(CliError::fatal(format!(
                "cannot {action}: another command is still publishing objects in this \
                 repository, and deleting underneath it could leave a reference to bytes that \
                 no longer exist"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(
                "wait for the other command (a long-running `libra code` session counts) to \
                 finish, then retry",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The lock table is shared by every test in this module, so they run
    /// serially — a leaked hold in one would otherwise look like contention
    /// in another.
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;

    /// `(depth, mode)` currently recorded for this repository, if any.
    fn held(storage: &Path) -> Option<(usize, Mode)> {
        let path = lock_path(storage);
        let file = open_lock_file(&path).ok()?;
        let key = lock_key(&path, &file).ok()?;
        locks().get(&key).map(|state| (state.depth, state.mode))
    }

    #[test]
    #[serial]
    fn two_publishers_share_the_lock() {
        let dir = tempdir().unwrap();
        let a = MaintenanceLock::shared(dir.path()).unwrap();
        let b = MaintenanceLock::shared(dir.path()).unwrap();
        assert_eq!(held(dir.path()).map(|(depth, _)| depth), Some(2));
        drop((a, b));
        assert_eq!(held(dir.path()), None, "both guards released");
    }

    /// A deletion phase that re-enters the lock (obliteration holds it across
    /// the tombstone write and then reaches `delete_payload`, which re-checks)
    /// must see its own hold, not block on itself for the full timeout and
    /// then report a conflict that does not exist.
    #[test]
    #[serial]
    fn an_exclusive_hold_is_re_entrant_within_the_process() {
        let dir = tempdir().unwrap();
        let outer = MaintenanceLock::exclusive_or_refuse(dir.path(), "delete").unwrap();
        let inner = MaintenanceLock::exclusive_or_refuse(dir.path(), "delete again")
            .expect("re-entering our own exclusive hold must not deadlock or refuse");
        assert_eq!(held(dir.path()), Some((2, Mode::Exclusive)));
        drop(inner);
        assert_eq!(
            held(dir.path()),
            Some((1, Mode::Exclusive)),
            "the inner guard must not release the lock"
        );
        drop(outer);
        assert_eq!(held(dir.path()), None);
    }

    /// A shared hold is NOT upgradable: another process may hold it shared
    /// too, so waiting would be waiting on ourselves. The refusal is
    /// immediate rather than after the timeout.
    #[test]
    #[serial]
    fn a_shared_hold_refuses_to_upgrade_immediately() {
        let dir = tempdir().unwrap();
        let _publisher = MaintenanceLock::shared(dir.path()).unwrap();
        let started = Instant::now();
        let upgrade = MaintenanceLock::try_exclusive(dir.path(), Duration::from_secs(30)).unwrap();
        assert!(upgrade.is_none(), "a shared hold cannot become exclusive");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "and must not sit out the timeout first"
        );
    }

    #[test]
    #[serial]
    fn refusal_carries_the_conflict_code() {
        let dir = tempdir().unwrap();
        let _publisher = MaintenanceLock::shared(dir.path()).unwrap();
        let error = MaintenanceLock::exclusive_or_refuse(dir.path(), "obliterate an object")
            .expect_err("a contended lock must refuse");
        assert_eq!(
            error.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
    }

    /// A hold on repository A says NOTHING about repository B. Treating the
    /// hold as process-wide let a process publish into B re-entrantly —
    /// without ever opening B's lock file — while another process deleted
    /// B's objects.
    #[test]
    #[serial]
    fn a_hold_on_one_repository_does_not_cover_another() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        let _exclusive_a = MaintenanceLock::exclusive_or_refuse(a.path(), "delete in A").unwrap();
        assert_eq!(held(a.path()).map(|(_, mode)| mode), Some(Mode::Exclusive));
        assert_eq!(held(b.path()), None, "B is untouched");

        // Taking B's lock must really take B's lock, not inherit A's.
        let _shared_b = MaintenanceLock::shared(b.path()).unwrap();
        assert_eq!(held(b.path()), Some((1, Mode::Shared)));
        // And B's shared hold still cannot be upgraded, independently of A.
        assert!(
            MaintenanceLock::try_exclusive(b.path(), Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "B's own shared hold governs B"
        );
    }

    /// Two aliases of one lock FILE are one hold, because `flock` is per
    /// inode.
    ///
    /// Keyed by pathname, an exclusive hold taken through one alias and a
    /// shared acquisition through the other looked like two repositories:
    /// the second would wait on the first's `flock` — while holding the very
    /// mutex the first needs to release it.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn two_paths_to_one_lock_file_are_one_hold() {
        let dir = tempdir().unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();

        let _exclusive = MaintenanceLock::exclusive_or_refuse(dir.path(), "delete").unwrap();
        // Through the OTHER spelling of the same directory.
        let started = Instant::now();
        let nested = MaintenanceLock::shared(&alias).expect("alias must re-enter, not block");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an alias must not wait on our own hold"
        );
        assert_eq!(held(dir.path()), Some((2, Mode::Exclusive)));
        drop(nested);
        assert_eq!(held(dir.path()), Some((1, Mode::Exclusive)));
    }

    /// If the lock file is REPLACED between two holders, each would flock a
    /// different inode and the exclusion would silently stop existing. That
    /// is refused rather than serialised on a file nobody else can see.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn a_replaced_lock_file_is_refused_rather_than_ignored() {
        let dir = tempdir().unwrap();
        let path = lock_path(dir.path());
        let file = open_lock_file(&path).unwrap();
        // Someone swaps the file out from under the opened descriptor.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();

        let error = lock_key(&path, &file).expect_err("a replaced lock file must be refused");
        assert_eq!(
            error.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
    }

    /// Hold → replace → a second acquisition through the same pathname is
    /// REFUSED, rather than quietly locking a different inode.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn a_replacement_under_a_live_hold_refuses_the_next_acquisition() {
        let dir = tempdir().unwrap();
        let path = lock_path(dir.path());
        let _held = MaintenanceLock::shared(dir.path()).expect("first hold");

        // The file behind the pathname is swapped out. Our descriptor still
        // flocks the old inode; a new acquisition would flock the new one.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();

        let error = MaintenanceLock::shared(dir.path())
            .expect_err("a second holder must not lock a different inode");
        assert_eq!(
            error.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        let deleter = MaintenanceLock::try_exclusive(dir.path(), Duration::from_millis(50));
        assert!(
            deleter.is_err(),
            "and neither may a deletion phase: the exclusion no longer exists"
        );
    }

    /// Cross-PROCESS exclusion is the property that actually matters, and it
    /// is what the integration test
    /// `gc_defers_deletion_while_a_publisher_holds_the_maintenance_lock`
    /// pins end to end (a real second process holds the lock while `gc`
    /// runs). Here we only assert that releasing everything leaves the file
    /// unlocked for the next acquirer.
    #[test]
    #[serial]
    fn releasing_the_last_guard_admits_the_next_acquirer() {
        let dir = tempdir().unwrap();
        let first = MaintenanceLock::try_exclusive(dir.path(), Duration::from_millis(50))
            .unwrap()
            .expect("uncontended");
        drop(first);
        let second = MaintenanceLock::try_exclusive(dir.path(), Duration::from_millis(50)).unwrap();
        assert!(second.is_some(), "the lock was released");
    }
}

#[cfg(all(test, feature = "test-live-agent"))]
mod capture_maintenance_local_tests {
    use std::{
        future::Future,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    use super::*;

    type Job<T> = (thread::JoinHandle<()>, mpsc::Receiver<thread::Result<T>>);

    fn launch<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Job<T> {
        let (send, receive) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("local lock runtime")
                    .block_on(future)
            }));
            let _ = send.send(result);
        });
        (worker, receive)
    }

    // The parent owns the raw blocker so even a broken synchronous flock can
    // be released before joining the worker. A Tokio-only timer could freeze
    // on that same executor and would not catch this regression.
    fn finish<T: Send + 'static>((worker, receive): Job<T>, blocker: Option<File>) -> T {
        let result = receive.recv_timeout(Duration::from_secs(3));
        if let Some(blocker) = blocker {
            let _ = blocker.unlock();
        }
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                if receive.recv_timeout(Duration::from_secs(3)).is_ok() {
                    worker.join().expect("join unblocked failed worker");
                    panic!("maintenance wait blocked its executor beyond the watchdog");
                }
                // A broken global-mutex implementation must fail this local
                // test process rather than strand every remaining test.
                eprintln!(
                    "maintenance watchdog could not reap its worker after releasing the owned blocker"
                );
                std::process::abort();
            }
        };
        worker.join().expect("join local lock worker");
        match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn raw_exclusive(storage: &Path) -> File {
        let file = open_lock_file(&lock_path(storage)).expect("owned lock file");
        file.lock().expect("raw unregistered exclusive holder");
        file
    }

    fn depth(storage: &Path) -> usize {
        let path = lock_path(storage);
        let file = open_lock_file(&path).expect("owned lock file");
        let key = lock_key(&path, &file).expect("owned lock key");
        locks().get(&key).map_or(0, |state| state.depth)
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_expired_wait_has_no_filesystem_side_effect() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().join("not-created");
        finish(
            launch(async move {
                assert!(
                    MaintenanceLock::shared_until(&storage, Instant::now())
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(!storage.exists());
            }),
            None,
        );
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_contention_deadline_keeps_current_thread_ticker_running() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let blocker = raw_exclusive(&storage);
        finish(
            launch(async move {
                let ticks = Arc::new(AtomicUsize::new(0));
                let ticker_ticks = ticks.clone();
                let ticker = tokio::spawn(async move {
                    loop {
                        ticker_ticks.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                });
                let started = Instant::now();
                let outcome =
                    MaintenanceLock::shared_until(&storage, started + Duration::from_millis(120))
                        .await
                        .unwrap();
                ticker.abort();
                let _ = ticker.await;
                assert!(outcome.is_none());
                assert!(started.elapsed() < Duration::from_secs(1));
                assert!(
                    ticks.load(Ordering::SeqCst) >= 2,
                    "waiting must yield its executor"
                );
                assert_eq!(depth(&storage), 0);
            }),
            Some(blocker),
        );
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_released_blocker_allows_shared_guard() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let blocker = raw_exclusive(&storage);
        let target = storage.clone();
        let (ready_send, ready_receive) = mpsc::sync_channel(1);
        let job = launch(async move {
            let notifier = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = ready_send.send(());
            });
            let result =
                MaintenanceLock::shared_until(&target, Instant::now() + Duration::from_secs(2))
                    .await
                    .unwrap();
            notifier.await.expect("waiter notification settled");
            result
        });
        if ready_receive.recv_timeout(Duration::from_secs(1)).is_err() {
            drop(blocker);
            let _ = finish(job, None);
            panic!("contended acquisition did not yield its current-thread runtime");
        }
        drop(blocker);
        let guard = finish(job, None).expect("released before original deadline");
        assert_eq!(depth(&storage), 1);
        let probe = open_lock_file(&lock_path(&storage)).expect("exclusive probe");
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(guard);
        assert_eq!(depth(&storage), 0);
        probe.try_lock().expect("last guard releases the OS lock");
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_cancelled_wait_leaves_no_hold() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let blocker = raw_exclusive(&storage);
        let target = storage.clone();
        finish(
            launch(async move {
                let mut pending = Box::pin(MaintenanceLock::shared_until(
                    &target,
                    Instant::now() + Duration::from_secs(2),
                ));
                tokio::select! {
                    _ = &mut pending => panic!("raw exclusive holder remains live"),
                    _ = tokio::time::sleep(Duration::from_millis(60)) => {}
                }
                drop(pending);
                assert_eq!(depth(&target), 0);
            }),
            Some(blocker),
        );
        let probe = open_lock_file(&lock_path(&storage)).expect("exclusive probe");
        probe.try_lock().expect("cancelled waiter retained no lock");
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_reentrant_exclusive_depth_and_expired_join() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let exclusive = MaintenanceLock::try_exclusive(&storage, Duration::ZERO)
            .unwrap()
            .expect("first owner");
        let target = storage.clone();
        let shared = finish(
            launch(async move {
                let shared =
                    MaintenanceLock::shared_until(&target, Instant::now() + Duration::from_secs(1))
                        .await
                        .unwrap()
                        .expect("join process exclusive hold");
                assert_eq!(depth(&target), 2);
                assert!(
                    MaintenanceLock::shared_until(&target, Instant::now())
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(depth(&target), 2, "expired join has no depth side effect");
                shared
            }),
            None,
        );
        drop(exclusive);
        assert_eq!(depth(&storage), 1);
        let probe = open_lock_file(&lock_path(&storage)).expect("exclusive probe");
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(shared);
        probe.try_lock().expect("both guards released");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_wait_rejects_replaced_inode() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let blocker = raw_exclusive(&storage);
        let target = storage.clone();
        let (ready_send, ready_receive) = mpsc::sync_channel(1);
        let job = launch(async move {
            let notifier = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = ready_send.send(());
            });
            let result =
                MaintenanceLock::shared_until(&target, Instant::now() + Duration::from_secs(2))
                    .await;
            notifier.await.expect("waiter notification settled");
            result
        });
        if ready_receive.recv_timeout(Duration::from_secs(1)).is_err() {
            drop(blocker);
            let _ = finish(job, None);
            panic!("replacement fixture must observe the waiter yield before renaming");
        }
        let replacement = directory.path().join("replacement-lock");
        File::create(&replacement).expect("owned replacement");
        std::fs::rename(&replacement, lock_path(&storage)).expect("replace named lock");
        drop(blocker);
        assert!(
            finish(job, None).is_err(),
            "an old inode cannot guard the new pathname"
        );
        assert_eq!(depth(&storage), 0);
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    #[cfg(unix)]
    fn capture_maintenance_wait_rechecks_name_before_joining_a_new_process_hold() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        let blocker = raw_exclusive(&storage);
        let child_blocker = blocker
            .try_clone()
            .expect("watchdog can release this fd group");
        finish(
            launch(async move {
                let mut pending = Box::pin(MaintenanceLock::shared_until(
                    &storage,
                    Instant::now() + Duration::from_secs(2),
                ));
                // Poll once without completing the acquisition: its original
                // open/key must now name the raw-exclusive-held inode.
                std::future::poll_fn(|context| match pending.as_mut().poll(context) {
                    std::task::Poll::Pending => std::task::Poll::Ready(()),
                    std::task::Poll::Ready(_) => panic!("exclusive blocker must force a wait"),
                })
                .await;
                let old_key = lock_key(&lock_path(&storage), &child_blocker).expect("old identity");
                child_blocker.unlock().expect("release the raw blocker");
                let old_hold =
                    MaintenanceLock::shared(&storage).expect("new process hold on old inode");
                let replacement = storage.join("replacement-before-join");
                File::create(&replacement).expect("owned replacement");
                std::fs::rename(replacement, lock_path(&storage)).expect("replace named inode");
                assert!(
                    pending.await.is_err(),
                    "joining the cached old key must not bypass the name check"
                );
                assert_eq!(locks().get(&old_key).map(|held| held.depth), Some(1));
                drop(old_hold);
                assert!(!locks().contains_key(&old_key));
            }),
            Some(blocker),
        );
    }

    #[test]
    #[ignore = "operator-local capture prerequisite; never run in CI"]
    fn capture_maintenance_two_threads_keep_one_accurate_guard_count() {
        let directory = tempfile::tempdir().expect("owned root");
        let storage = directory.path().to_path_buf();
        for _ in 0..8 {
            let ready = Arc::new(AtomicBool::new(false));
            let start = |target: PathBuf, ready: Arc<AtomicBool>| {
                launch(async move {
                    while !ready.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                    MaintenanceLock::shared_until(&target, Instant::now() + Duration::from_secs(1))
                        .await
                        .unwrap()
                        .expect("compatible shared acquisition")
                })
            };
            let first = start(storage.clone(), ready.clone());
            let second = start(storage.clone(), ready.clone());
            ready.store(true, Ordering::SeqCst);
            let first = finish(first, None);
            let second = finish(second, None);
            assert_eq!(depth(&storage), 2);
            let probe = open_lock_file(&lock_path(&storage)).expect("raw exclusive probe");
            assert!(matches!(
                probe.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            drop(first);
            assert_eq!(depth(&storage), 1);
            assert!(
                matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "dropping one fd must not unlock the surviving guard"
            );
            drop(second);
            assert_eq!(depth(&storage), 0);
            probe.try_lock().expect("both live guards released");
        }
    }
}
