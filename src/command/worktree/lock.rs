//! Registry mutation lock; callers hold it through registry and SQLite writes.

use std::fs;

use super::{WorktreeError, WorktreeResult};
use crate::utils::util;

/// RAII guard over the worktree REGISTRY mutation lock (`worktrees.lock` in
/// the common storage). Serializes every registry mutator's
/// load → check → mutate → write sequence across processes: without it, a
/// concurrent `worktree add`'s strict pre-seed sweep could delete rows
/// another add just seeded for the same deterministic instance id, and two
/// load/modify/write registry updates could drop each other's entries. The
/// flock is BLOCKING (concurrent mutators queue rather than fail) and
/// released on drop (or process exit). Read-only paths (`list`) stay
/// lock-free.
pub(crate) struct RegistryLockGuard {
    file: fs::File,
}

impl Drop for RegistryLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

///
/// PRIVATE ON PURPOSE (plan-20260714 W1): the blocking variant has
/// exactly ONE caller — the `spawn_blocking` in
/// [`acquire_registry_lock_async`]. Every other acquisition in the
/// crate, sync or async, goes through that helper, so the
/// blocking-on-a-runtime-worker deadlock cannot be reintroduced from
/// another module by accident.
fn acquire_registry_lock() -> WorktreeResult<RegistryLockGuard> {
    let lock_path = util::storage_path().join("worktrees.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            WorktreeError::IoWrite(format!(
                "cannot open the worktree registry lock '{}': {e}",
                lock_path.display()
            ))
        })?;
    // std file locking is CROSS-PLATFORM (flock on Unix, LockFileEx on
    // Windows) and BLOCKING — concurrent mutators queue rather than fail.
    file.lock().map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot lock the worktree registry '{}': {e}",
            lock_path.display()
        ))
    })?;
    Ok(RegistryLockGuard { file })
}

/// [`acquire_registry_lock`] for ASYNC callers: takes the blocking `flock`
/// on the blocking pool instead of on a runtime worker.
///
/// Blocking a worker here is not merely impolite, it is a LIVENESS BUG.
/// sqlx returns a pooled connection by SPAWNING a task; a spawn from inside
/// a poll lands in that worker's non-stealable LIFO slot; and sea-orm pins
/// SQLite pools to ONE connection. A worker blocked right after a query
/// therefore strands the connection return, and every database user in the
/// process — including whoever holds this very lock — waits out the full
/// sqlx acquire timeout for a connection that can never come back. The
/// service's dirty-mark handler deadlocked exactly that way.
///
/// The returned guard owns only a `File`, so it is `Send` and may be held
/// across subsequent awaits: the registry → SQLite lock ORDER is deliberate
/// (validate and write under one hold). Only the ACQUISITION must leave the
/// runtime worker.
pub(crate) async fn acquire_registry_lock_async() -> WorktreeResult<RegistryLockGuard> {
    match tokio::task::spawn_blocking(acquire_registry_lock).await {
        Ok(result) => result,
        Err(error) => Err(WorktreeError::IoWrite(format!(
            "the worktree registry lock task failed: {error}"
        ))),
    }
}
