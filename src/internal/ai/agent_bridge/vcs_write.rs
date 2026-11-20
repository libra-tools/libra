//! `vcs.stage`, `vcs.unstage`, and `vcs.commit` (plan-20261118 LVC-06).
//!
//! These do not reuse `commit.create`. Each call names literal repository
//! paths, an `operation_id`, and the HEAD/index/worktree fence the caller
//! observed. A stale fence returns before any index or commit write.

use std::{collections::BTreeSet, path::Path};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{protocol::BridgeError, vcs};
use crate::{
    command::{
        restore::{RestoreArgs, execute_safe as restore_execute},
        status::{changes_to_be_committed_safe, changes_to_be_staged},
    },
    utils::{path, util},
};

const MAX_FENCE_PATHS: usize = 256;
const MAX_STAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CASE_DIR_ENTRIES: usize = 100_000;
static WRITE_GATE: Mutex<()> = Mutex::const_new(());
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FENCE_RECHECK_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static STAGE_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static MODE_CHMOD_BETWEEN_READS: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static BREAK_SECOND_STAGE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static BREAK_AT_PUBLISH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static BREAK_ON_FIRST_PUBLISH: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static STAGE_ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FAIL_SECOND_PUBLISH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static PUBLISH_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static PUBLISH_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static PUBLISH_REMOVE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static MODE_STAT_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Test seam: rewrite this file after the first fence check and before the
/// second. Not used by the protocol.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_fence_recheck_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *FENCE_RECHECK_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: rewrite this file after the pre-stage snapshot and before `add`.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_stage_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *STAGE_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: clear the executable bit between the two content reads.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn fail_second_publish_for_test(enabled: bool) {
    PUBLISH_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    FAIL_SECOND_PUBLISH.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_break_on_first_publish_for_test(path: Option<std::path::PathBuf>) {
    *BREAK_ON_FIRST_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_break_at_publish_for_test(path: Option<std::path::PathBuf>) {
    *BREAK_AT_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_break_second_stage_for_test(path: Option<std::path::PathBuf>) {
    STAGE_ATTEMPTS.store(0, std::sync::atomic::Ordering::SeqCst);
    *BREAK_SECOND_STAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_mode_chmod_between_reads_for_test(path: Option<std::path::PathBuf>) {
    *MODE_CHMOD_BETWEEN_READS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: rewrite this file after the snapshot matches and before the
/// approved bytes are stored.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_publish_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *PUBLISH_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: remove this file after the snapshot is confirmed and before publish.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_publish_remove_for_test(path: Option<std::path::PathBuf>) {
    *PUBLISH_REMOVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: replace this file after the content reread and before the path stat.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_mode_stat_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *MODE_STAT_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn rewrite_before_mode_stat() {
    let Some(path) = MODE_STAT_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::write(path, b"changed before mode stat\n");
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn remove_during_publish() {
    let Some(path) = PUBLISH_REMOVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::remove_file(path);
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn rewrite_during_publish() {
    let Some(path) = PUBLISH_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::write(path, b"changed while publishing\n");
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn break_on_first_publish() {
    let Some(path) = BREAK_ON_FIRST_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    *BREAK_ON_FIRST_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::create_dir(&path);
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn break_at_publish() {
    let Some(path) = BREAK_AT_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    *BREAK_AT_PUBLISH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::create_dir(&path);
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn note_stage_attempt() {
    let attempt = STAGE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if attempt == 0 {
        return;
    }
    let Some(path) = BREAK_SECOND_STAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::create_dir(&path);
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn chmod_between_reads() {
    let Some(path) = MODE_CHMOD_BETWEEN_READS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    #[cfg(unix)]
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o644));
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn rewrite_staged_path() {
    let Some(path) = STAGE_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::write(path, b"changed while staging\n");
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
fn rewrite_for_fence_recheck() {
    let Some(path) = FENCE_RECHECK_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    else {
        return;
    };
    let _ = std::fs::write(path, b"changed after the first fence check\n");
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn rewrite_before_mode_stat() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn remove_during_publish() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn rewrite_during_publish() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn break_on_first_publish() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn break_at_publish() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn note_stage_attempt() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn chmod_between_reads() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn rewrite_staged_path() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn rewrite_for_fence_recheck() {}

/// Serialize in-process bridge publications. `MaintenanceLock::shared` joins an
/// exclusive hold already owned by this process, so it cannot keep another
/// bridge task out. This gate can. Callers that already hold it must use the
/// unlocked publication path.
pub async fn acquire_publish_gate() -> tokio::sync::MutexGuard<'static, ()> {
    WRITE_GATE.lock().await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFence {
    pub head: String,
    pub index_generation: String,
    pub worktree_generation: String,
}

pub async fn current_commit_fence() -> Result<(String, String), BridgeError> {
    let head = vcs::read_fence()
        .await?
        .head
        .map(|oid| oid.to_string())
        .unwrap_or_default();
    let index_path = path::try_index().map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: bridge commit needs a Libra index")
    })?;
    let index_generation = sha256_hex(&read_index_bounded(&index_path)?);
    Ok((head, index_generation))
}

pub async fn current_write_fence() -> Result<WriteFence, BridgeError> {
    let (head, index_generation) = current_commit_fence().await?;
    let workdir = util::try_working_dir().map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: vcs write methods need a Libra worktree")
    })?;
    Ok(WriteFence {
        head,
        index_generation,
        worktree_generation: worktree_generation(&workdir).await?,
    })
}

fn read_index_bounded(index: &Path) -> Result<Vec<u8>, BridgeError> {
    use std::io::Read;
    const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
    let parent = index
        .parent()
        .ok_or_else(|| BridgeError::internal("vcs fence: index has no parent"))?;
    let name = index
        .file_name()
        .ok_or_else(|| BridgeError::internal("vcs fence: index has no file name"))?;
    let root = crate::utils::beneath::open_root(parent).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs fence: cannot pin index directory", error)
    })?;
    let relative = Path::new(name);
    let before =
        crate::utils::beneath::entry_identity_beneath(&root, relative).map_err(|error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot identify index", error)
        })?;
    let file =
        crate::utils::beneath::open_file_beneath_nonblocking(&root, relative).map_err(|error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot open index", error)
        })?;
    let opened = crate::utils::beneath::identity_for_file(&file).map_err(|error| {
        vcs::stale_fence_hiding_host_paths("vcs fence: cannot identify open index", error)
    })?;
    if before != opened {
        return Err(BridgeError::stale_fence(
            "vcs fence: index replaced before reading",
        ));
    }
    let metadata_before = file
        .metadata()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: cannot stat index", error))?;
    let snapshot_before = crate::utils::beneath::RawLstat::from_metadata(&metadata_before)
        .map_err(|error| {
            vcs::internal_hiding_host_paths("vcs fence: cannot inspect index", error)
        })?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: cannot read index", error))?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(BridgeError::invalid_params(
            "vcs fence: index exceeds byte budget",
        ));
    }
    let metadata_after = file.metadata().map_err(|error| {
        vcs::internal_hiding_host_paths("vcs fence: cannot restat index", error)
    })?;
    let snapshot_after =
        crate::utils::beneath::RawLstat::from_metadata(&metadata_after).map_err(|error| {
            vcs::internal_hiding_host_paths("vcs fence: cannot inspect index", error)
        })?;
    let path_after =
        crate::utils::beneath::entry_identity_beneath(&root, relative).map_err(|error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot reidentify index", error)
        })?;
    if opened != path_after
        || snapshot_before.mode != snapshot_after.mode
        || snapshot_before.len != snapshot_after.len
        || snapshot_before.mtime_sec != snapshot_after.mtime_sec
        || snapshot_before.mtime_nsec != snapshot_after.mtime_nsec
        || snapshot_before.ctime_sec != snapshot_after.ctime_sec
        || snapshot_before.ctime_nsec != snapshot_after.ctime_nsec
    {
        return Err(BridgeError::stale_fence(
            "vcs fence: index changed while reading",
        ));
    }
    Ok(bytes)
}

/// Re-read the fence and run `write` while this process holds the write gate.
/// Another process can still change the repository; that residual race is not
/// closed by this gate.
pub fn acquire_repository_write_lock()
-> Result<crate::internal::maintenance_lock::MaintenanceLock, BridgeError> {
    let storage = util::try_get_storage_path(None).map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: bridge writes need Libra storage")
    })?;
    crate::internal::maintenance_lock::MaintenanceLock::try_exclusive(
        &storage,
        std::time::Duration::ZERO,
    )
    .map_err(|error| vcs::internal_hiding_host_paths("bridge write: acquire repository lock", error))?
    .ok_or_else(|| {
        BridgeError::stale_fence(
            "stale_state: another Libra operation is publishing in this repository; refresh and retry",
        )
    })
}

pub async fn acquire_commit_publish_lock()
-> Result<Option<crate::internal::maintenance_lock::MaintenanceLock>, BridgeError> {
    if !super::publication::bridge_commit_lock_required() {
        return Ok(None);
    }
    let (expected_head, expected_index, expected_worktree) =
        super::publication::expected_commit_fence().ok_or_else(|| {
            BridgeError::internal("bridge commit publication is missing its admitted fence")
        })?;
    let publishers = acquire_repository_write_lock()?;
    let (actual_head, actual_index, actual_worktree) = if expected_worktree.is_some() {
        let actual = current_write_fence().await?;
        (
            actual.head,
            actual.index_generation,
            Some(actual.worktree_generation),
        )
    } else {
        let (head, index) = current_commit_fence().await?;
        (head, index, None)
    };
    if actual_head != expected_head
        || expected_index
            .as_ref()
            .is_some_and(|expected| actual_index != *expected)
        || expected_worktree
            .as_ref()
            .is_some_and(|expected| actual_worktree.as_deref() != Some(expected.as_str()))
    {
        return Err(BridgeError::stale_fence(
            "stale_state: repository state changed while commit hooks ran; refresh and retry",
        ));
    }
    Ok(Some(publishers))
}

pub async fn run_fenced<T>(
    expected: &WriteFence,
    hold_lock_during_write: bool,
    write: impl std::future::Future<Output = Result<T, BridgeError>>,
) -> Result<T, BridgeError> {
    let _gate = acquire_publish_gate().await;
    // Libra commands hold this lock shared. Taking it exclusively validates the
    // admitted fence against other processes. Staging and unstaging keep it
    // through publication; commits release it for hooks and reacquire at the
    // final object/ref publication point.
    let publishers = acquire_repository_write_lock()?;
    let actual = current_write_fence().await?;
    require_matching_fence(
        &actual,
        &expected.head,
        &expected.index_generation,
        &expected.worktree_generation,
    )?;
    rewrite_for_fence_recheck();
    let before_write = current_write_fence().await?;
    require_matching_fence(
        &before_write,
        &expected.head,
        &expected.index_generation,
        &expected.worktree_generation,
    )?;
    if hold_lock_during_write {
        write.await
    } else {
        drop(publishers);
        write.await
    }
}

pub fn require_matching_fence(
    actual: &WriteFence,
    expected_head: &str,
    expected_index: &str,
    expected_worktree: &str,
) -> Result<(), BridgeError> {
    if actual.head == expected_head
        && actual.index_generation == expected_index
        && actual.worktree_generation == expected_worktree
    {
        return Ok(());
    }
    Err(BridgeError::stale_fence(format!(
        "stale_state: expected fence head='{expected_head}' index='{expected_index}' worktree='{expected_worktree}' does not match head='{}' index='{}' worktree='{}'. No index or commit write was performed.",
        actual.head, actual.index_generation, actual.worktree_generation
    )))
}

/// A cross-process claim on bridge write execution for this repository.
///
/// A single stable lock file coordinates bridge writers and reconcilers across
/// processes. This serializes their side effects but does not use the
/// maintenance lock: a commit runs repository hooks, and a hook may start
/// another Libra command, which must not deadlock against the bridge's own
/// publication.
pub struct BridgeWriteClaim {
    _file: std::fs::File,
}

impl BridgeWriteClaim {
    /// Take the repository claim, waiting up to `budget` for a holder.
    ///
    /// A caller whose own side effects are idempotent and quick uses this so
    /// that two such callers in one process do not refuse each other. When the
    /// budget expires the claim is refused exactly as a nonblocking attempt
    /// would be.
    pub async fn acquire_within(budget: std::time::Duration) -> Result<Option<Self>, BridgeError> {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if let Some(claim) = Self::try_acquire()? {
                return Ok(Some(claim));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Try to take the repository claim without waiting.
    ///
    /// A crashed owner releases the lock when its process exits. The stable
    /// claim file is never removed, so concurrent callers cannot lock
    /// different inodes for the same repository.
    pub fn try_acquire() -> Result<Option<Self>, BridgeError> {
        let path = claim_path()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                vcs::internal_hiding_host_paths("bridge operation claim: open lock file", error)
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(vcs::internal_hiding_host_paths(
                "bridge operation claim: lock",
                error,
            )),
        }
    }
}

/// The stable repository claim file for bridge writers.
fn claim_path() -> Result<std::path::PathBuf, BridgeError> {
    let storage = util::try_get_storage_path(None).map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: bridge writes need Libra storage")
    })?;
    Ok(storage.join("agent-bridge-writes.lock"))
}

pub async fn stage_paths(paths: &[String]) -> Result<PreparedIndexWrite, BridgeError> {
    apply_each(paths, true).await
}

pub async fn unstage_paths(paths: &[String]) -> Result<PreparedIndexWrite, BridgeError> {
    apply_each(paths, false).await
}

/// A prepared index publication.
///
/// The operation is split in two so the caller can record a durable intent
/// naming the EXACT index effect before the live index is replaced. The
/// publication itself is recorded in two phases: 'prepared' before the
/// replacement and 'published' after it. Only the 'published' phase proves
/// that this operation installed the index; a crash between the two phases
/// leaves the outcome genuinely uncertain, because the replacement may or may
/// not have happened before the bridge stopped.
pub struct PreparedIndexWrite {
    bytes: Vec<u8>,
    digest: String,
    applied: Vec<String>,
    index_path: std::path::PathBuf,
    expected_snapshot: Vec<u8>,
}

impl PreparedIndexWrite {
    /// The digest of the index bytes this publication will install.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The paths this operation applied.
    pub fn applied(&self) -> &[String] {
        &self.applied
    }

    /// Install the prepared index.
    ///
    /// The exclusive repository lock is the publication protocol shared with
    /// every supported Libra index writer: each of them takes the same lock
    /// shared for its whole command, so no supported writer can replace the
    /// index while this publication runs. The comparison below is a second
    /// line of defence for a writer that ignores that lock; it cannot make
    /// the replacement conditional, so a writer outside the protocol can still
    /// lose its update in the window between the comparison and the rename.
    /// The published bytes are verified afterwards, which reports a writer
    /// that replaced the index after this publication.
    pub fn publish(&self) -> Result<(), BridgeError> {
        let live = read_index_bounded(&self.index_path)?;
        if live != self.expected_snapshot {
            return Err(BridgeError::stale_fence(
                "stale_state: index changed before publication; no index write was performed",
            ));
        }
        crate::utils::atomic_write::write_atomic(&self.index_path, &self.bytes, true)
            .map_err(|error| vcs::internal_hiding_host_paths("vcs write: publish index", error))?;
        let published = read_index_bounded(&self.index_path)?;
        if published != self.bytes {
            return Err(BridgeError::stale_fence(
                "stale_state: the index was replaced by another writer during publication",
            ));
        }
        Ok(())
    }
}

fn collapse_equivalent_spellings(paths: &mut std::collections::BTreeSet<String>) {
    let names: Vec<String> = paths.iter().cloned().collect();
    let mut groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for name in names {
        let Ok(on_disk) = on_disk_spelling(&name) else {
            continue;
        };
        groups.entry(on_disk).or_default().push(name);
    }
    let Ok(workdir) = util::try_working_dir() else {
        return;
    };
    if !crate::utils::path_case::probe_dir_ignore_case(&workdir) {
        return;
    }
    let Ok(root) = crate::utils::beneath::open_root(&workdir) else {
        return;
    };
    for (on_disk, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let same_entry = members.iter().all(|member| {
            member == &on_disk
                || crate::utils::beneath::same_nofollow_entry(
                    &root,
                    Path::new(&on_disk),
                    Path::new(member),
                )
                .ok()
                    == Some(true)
        });
        if !same_entry {
            continue;
        }
        for member in members {
            paths.remove(&member);
        }
        paths.insert(on_disk);
    }
}

pub async fn commit_index(
    message: &str,
    requested: &[String],
) -> Result<serde_json::Value, BridgeError> {
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: read index", error))?;
    let relative = staged.to_relative();
    let mut staged_paths = std::collections::BTreeSet::new();
    for path in relative
        .new
        .iter()
        .chain(relative.modified.iter())
        .chain(relative.deleted.iter())
    {
        staged_paths.insert(path.display().to_string().replace('\\', "/"));
    }
    collapse_equivalent_spellings(&mut staged_paths);
    for (_old, new) in &relative.renamed {
        staged_paths.insert(new.display().to_string().replace('\\', "/"));
    }
    let requested_paths = requested
        .iter()
        .map(|path| on_disk_spelling(&path.replace('\\', "/")))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    if staged_paths != requested_paths {
        return Err(BridgeError::invalid_params(format!(
            "vcs.commit paths {requested_paths:?} do not match the staged set {staged_paths:?}; refusing before creating a commit"
        )));
    }
    let created = vcs::commit_create_unguarded(message, false, false).await?;
    Ok(serde_json::json!({
        "commitOid": created["commit"],
        "paths": staged_paths.into_iter().collect::<Vec<_>>(),
    }))
}

async fn apply_each(paths: &[String], stage: bool) -> Result<PreparedIndexWrite, BridgeError> {
    let spelled = paths
        .iter()
        .map(|path| on_disk_spelling(path))
        .collect::<Result<Vec<_>, _>>()?;
    let paths = spelled.as_slice();
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    let mut first_error = None;
    let index_path = path::try_index().map_err(|error| {
        vcs::internal_hiding_host_paths("vcs write: cannot locate index snapshot", error)
    })?;
    let index_snapshot = read_index_bounded(&index_path)?;
    if stage {
        for path in paths {
            note_stage_attempt();
            preflight_stage_path(path)?;
        }
        break_at_publish();
        for path in paths {
            preflight_stage_path(path)?;
        }
        let mut plans = Vec::with_capacity(paths.len());
        for path in paths {
            plans.push(capture_stage_plan(path)?);
        }
        break_on_first_publish();
        for plan in &plans {
            confirm_stage_plan(plan)?;
        }
        rewrite_before_mode_stat();
        rewrite_during_publish();
        remove_during_publish();
        let mut index = git_internal::internal::index::Index::load(&index_path)
            .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: load index", error))?;
        if read_index_bounded(&index_path)? != index_snapshot {
            return Err(BridgeError::stale_fence(
                "stale_state: index changed while preparing stage",
            ));
        }
        // Every selected path is resolved before anything is stored, so one
        // refused path cannot leave another path's blob in the object store.
        resolve_stage_conversions(&mut plans, &index)?;
        for plan in &plans {
            if crate::utils::ignore::should_ignore(
                Path::new(plan.path()),
                crate::utils::ignore::IgnorePolicy::Respect,
                &index,
            ) {
                return Err(BridgeError::invalid_params(format!(
                    "vcs.stage: {} is ignored; refusing to stage it without force",
                    plan.path()
                )));
            }
        }
        for plan in plans {
            reject_second_publish()?;
            let path = plan.path().to_string();
            publish_stage_plan(plan, &mut index).await?;
            applied.push(path);
        }
        if read_index_bounded(&index_path)? != index_snapshot {
            return Err(BridgeError::stale_fence(
                "stale_state: index changed before stage publication",
            ));
        }
        let bytes = serialize_index(&index)?;
        return Ok(PreparedIndexWrite {
            digest: sha256_hex(&bytes),
            bytes,
            applied,
            index_path: index_path.clone(),
            expected_snapshot: index_snapshot,
        });
    }
    let parent = index_path
        .parent()
        .ok_or_else(|| BridgeError::internal("vcs.unstage: index has no parent directory"))?;
    let scratch = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs.unstage: create temporary index", error)
    })?;
    std::fs::write(scratch.path(), &index_snapshot).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs.unstage: initialize temporary index", error)
    })?;
    for selected in paths {
        let output = quiet_output();
        let result = path::with_index_override(
            scratch.path().to_path_buf(),
            restore_execute(
                RestoreArgs {
                    pathspec: vec![selected.clone()],
                    staged: true,
                    source: Some("HEAD".to_string()),
                    ..RestoreArgs::default()
                },
                &output,
            ),
        )
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("did not match any files") {
                BridgeError::invalid_params("vcs.unstage: path did not match any files")
            } else {
                vcs::internal_hiding_host_paths("vcs.unstage: restore index", error)
            }
        });
        match result {
            Ok(()) => applied.push(selected.clone()),
            Err(error) => {
                failed.push(serde_json::json!({"path": selected, "message": error.message}));
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if applied.is_empty() {
        return Err(BridgeError::internal("vcs write applied no paths"));
    }
    let staged_bytes = read_index_bounded(scratch.path())?;
    if read_index_bounded(&index_path)? != index_snapshot {
        return Err(BridgeError::stale_fence(
            "stale_state: index changed before unstage publication",
        ));
    }
    Ok(PreparedIndexWrite {
        digest: sha256_hex(&staged_bytes),
        bytes: staged_bytes,
        applied,
        index_path: index_path.clone(),
        expected_snapshot: index_snapshot,
    })
}

/// Repository metadata is never storable content. Ordinary `libra add`
/// skips it; the bridge refuses it so a caller cannot stage the repository's
/// own bookkeeping into the index.
fn refuse_repository_metadata(path: &str) -> Result<(), BridgeError> {
    let workdir = util::try_working_dir()
        .map_err(|_| BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"))?;
    let candidate = workdir.join(path);
    if crate::utils::worktree_layout::is_worktree_metadata(&candidate, &workdir) {
        return Err(BridgeError::invalid_params(format!(
            "vcs.stage: {path} is repository metadata and is never staged"
        )));
    }
    Ok(())
}

fn preflight_stage_path(path: &str) -> Result<(), BridgeError> {
    refuse_repository_metadata(path)?;
    if symlink_target(path)?.is_some() {
        return Ok(());
    }
    match read_optional_worktree_bytes(path)? {
        Some(_) => Ok(()),
        None => {
            let index_path = path::try_index().map_err(|error| {
                vcs::internal_hiding_host_paths("vcs.stage: locate index", error)
            })?;
            let index =
                git_internal::internal::index::Index::load(&index_path).map_err(|error| {
                    vcs::internal_hiding_host_paths("vcs.stage: reload index", error)
                })?;
            if (0..4).any(|stage| index.get(path, stage).is_some()) {
                Ok(())
            } else {
                Err(BridgeError::invalid_params(format!(
                    "vcs.stage: {path} is not tracked, so its absence is not a deletion"
                )))
            }
        }
    }
}

enum StagePlan {
    File {
        path: String,
        bytes: Vec<u8>,
        /// The bytes that will be stored, after the current text/eol policy
        /// and the filter check. Resolved before any blob is written.
        clean: Vec<u8>,
        mode: u32,
    },
    Link {
        path: String,
        target: Vec<u8>,
    },
    Delete {
        path: String,
    },
}

impl StagePlan {
    fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Link { path, .. } | Self::Delete { path } => path,
        }
    }
}

/// Capture the bytes a path will be staged from.
///
/// The snapshot is read through a pinned, no-follow descriptor, so a symlinked
/// ancestor cannot redirect the read outside the worktree. The snapshot is the
/// content this operation publishes: a later edit by a writer that ignores
/// Libra's lock is detected by the NEXT fence read, not by this stage, which
/// is the same snapshot semantics ordinary `libra add` has.
fn capture_stage_plan(path: &str) -> Result<StagePlan, BridgeError> {
    if let Some(target) = symlink_target(path)? {
        let again = symlink_target(path)?;
        if again.as_deref() != Some(target.as_slice()) {
            return Err(BridgeError::stale_fence(format!(
                "stale_state: symlink {path} changed before staging"
            )));
        }
        return Ok(StagePlan::Link {
            path: path.to_string(),
            target,
        });
    }
    let Some((snapshot_bytes, snapshot_mode)) = read_optional_worktree_bytes(path)? else {
        if read_optional_worktree_bytes(path)?.is_some() {
            return Err(BridgeError::stale_fence(format!(
                "stale_state: {path} appeared before its deletion was staged"
            )));
        }
        let index_path = path::try_index()
            .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: locate index", error))?;
        let index = git_internal::internal::index::Index::load(&index_path)
            .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: reload index", error))?;
        if !(0..4).any(|stage| index.get(path, stage).is_some()) {
            return Err(BridgeError::invalid_params(format!(
                "vcs.stage: {path} is not tracked, so its absence is not a deletion"
            )));
        }
        return Ok(StagePlan::Delete {
            path: path.to_string(),
        });
    };
    rewrite_staged_path();
    chmod_between_reads();
    let Some((reread, mode)) = read_optional_worktree_bytes(path)? else {
        return Err(BridgeError::stale_fence(format!(
            "stale_state: {path} disappeared before staging; no index or object write was performed"
        )));
    };
    if reread != snapshot_bytes || mode != snapshot_mode {
        return Err(BridgeError::stale_fence(format!(
            "stale_state: {path} changed before staging; no index or object write was performed"
        )));
    }
    Ok(StagePlan::File {
        path: path.to_string(),
        bytes: snapshot_bytes,
        clean: Vec::new(),
        mode: snapshot_mode,
    })
}

/// Resolve the stored representation for every selected path before a single
/// blob is written.
///
/// A refusal (an unsupported content filter, an oversized path, a text
/// conversion failure) must not leave earlier paths' blobs in the object
/// store, so the whole request is resolved first and published afterwards.
fn resolve_stage_conversions(
    plans: &mut [StagePlan],
    index: &git_internal::internal::index::Index,
) -> Result<(), BridgeError> {
    let workdir = util::try_working_dir()
        .map_err(|_| BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"))?;
    for plan in plans.iter_mut() {
        let StagePlan::File {
            path, bytes, clean, ..
        } = plan
        else {
            continue;
        };
        let selected = crate::command::status_io_worker::selected_index_oid(index, Path::new(path));
        let converted = crate::utils::worktree_conversion::clean_captured_bytes(
            &workdir,
            Path::new(path),
            bytes.clone(),
            || crate::command::status_io_worker::index_has_text_crlf(selected),
        )
        .map_err(map_stage_conversion_error)?;
        *clean = converted;
    }
    Ok(())
}

fn confirm_stage_plan(plan: &StagePlan) -> Result<(), BridgeError> {
    match plan {
        StagePlan::File {
            path, bytes, mode, ..
        } => match read_optional_worktree_bytes(path)? {
            Some((now, now_mode)) if now == *bytes && now_mode == *mode => Ok(()),
            Some(_) => Err(BridgeError::stale_fence(format!(
                "stale_state: {path} changed after its snapshot; no index or object write was performed"
            ))),
            None => Err(BridgeError::stale_fence(format!(
                "stale_state: {path} disappeared after its snapshot; no index or object write was performed"
            ))),
        },
        StagePlan::Link { path, target } => {
            if symlink_target(path)?.as_deref() == Some(target.as_slice()) {
                Ok(())
            } else {
                Err(BridgeError::stale_fence(format!(
                    "stale_state: symlink {path} changed after its snapshot; no index or object write was performed"
                )))
            }
        }
        StagePlan::Delete { path } => {
            if read_optional_worktree_bytes(path)?.is_some() {
                Err(BridgeError::stale_fence(format!(
                    "stale_state: {path} appeared after its deletion snapshot"
                )))
            } else {
                Ok(())
            }
        }
    }
}

/// Deliberate filter refusals stay path-free. Every other conversion failure
/// can embed a host path, so its detail is withheld.
fn map_stage_conversion_error(error: std::io::Error) -> BridgeError {
    if error.kind() == std::io::ErrorKind::Unsupported {
        let message = match error.to_string().as_str() {
            "LFS snapshot staging is not supported" => {
                "vcs.stage: LFS snapshot staging is not supported; no index or object write was performed"
            }
            "content filter staging is not supported" => {
                "vcs.stage: content filter staging is not supported; no index or object write was performed"
            }
            _ => {
                return vcs::internal_hiding_host_paths("vcs.stage: convert snapshot", error);
            }
        };
        return BridgeError::invalid_params(message);
    }
    vcs::internal_hiding_host_paths("vcs.stage: convert snapshot", error)
}

fn reject_second_publish() -> Result<(), BridgeError> {
    #[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
    let call = PUBLISH_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
    if FAIL_SECOND_PUBLISH.load(std::sync::atomic::Ordering::SeqCst) && call == 1 {
        return Err(BridgeError::invalid_params(
            "vcs.stage: injected failure before the second publish",
        ));
    }
    Ok(())
}

async fn publish_stage_plan(
    plan: StagePlan,
    index: &mut git_internal::internal::index::Index,
) -> Result<(), BridgeError> {
    use git_internal::internal::object::types::ObjectType;
    match plan {
        // The stored representation was resolved for every selected path
        // before publication began, so this step cannot refuse.
        StagePlan::File {
            path, clean, mode, ..
        } => {
            let snapshot =
                git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, &clean);
            publish_snapshot(&path, index, &clean, snapshot, mode).await
        }
        StagePlan::Link { path, target } => {
            let snapshot =
                git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, &target);
            publish_snapshot(&path, index, &target, snapshot, 0o120000).await
        }
        StagePlan::Delete { path } => stage_tracked_deletion(&path, index),
    }
}

async fn publish_snapshot(
    path: &str,
    index: &mut git_internal::internal::index::Index,
    bytes: &[u8],
    snapshot: git_internal::hash::ObjectHash,
    mode: u32,
) -> Result<(), BridgeError> {
    use git_internal::internal::{index::IndexEntry, object::blob::Blob};

    use crate::utils::object_ext::BlobExt;

    let blob = Blob::from_content_bytes(bytes.to_vec());
    if blob.id != snapshot {
        return Err(BridgeError::internal(
            "vcs.stage: snapshot hash does not match the blob",
        ));
    }
    blob.try_save()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: store blob", error))?;
    let workdir = util::try_working_dir()
        .map_err(|_| BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"))?;
    remove_case_aliases(index, path)?;
    let mut entry =
        match crate::command::verified_index_entry(Path::new(path), blob.id, &workdir, None) {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                IndexEntry::new_from_blob(
                    path.to_string(),
                    blob.id,
                    u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                )
            }
            Err(error) => {
                return Err(vcs::internal_hiding_host_paths(
                    "vcs.stage: index entry",
                    error,
                ));
            }
        };
    entry.mode = mode;
    crate::utils::index_ext::update_preserving_file_mode_except_intent(index, entry, true);
    Ok(())
}

fn symlink_target(relative: &str) -> Result<Option<Vec<u8>>, BridgeError> {
    let workdir = util::try_working_dir()
        .map_err(|_| BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"))?;
    let root = crate::utils::beneath::open_root(&workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: pin root", error))?;
    let path = Path::new(relative);
    match crate::utils::beneath::lstat_beneath(&root, path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotADirectory
                || error.kind() == std::io::ErrorKind::InvalidInput =>
        {
            Err(BridgeError::invalid_params(format!(
                "vcs.stage: {relative} is not a regular file or symlink"
            )))
        }
        Err(error) => Err(vcs::internal_hiding_host_paths(
            &format!("vcs.stage: inspect {relative}"),
            error,
        )),
        Ok(metadata) if metadata.is_symlink => {
            crate::utils::beneath::read_symlink_beneath(&root, path)
                .map(Some)
                .map_err(|error| {
                    vcs::internal_hiding_host_paths(
                        &format!("vcs.stage: read symlink {relative}"),
                        error,
                    )
                })
        }
        Ok(_) => Ok(None),
    }
}

fn remove_case_aliases(
    index: &mut git_internal::internal::index::Index,
    path: &str,
) -> Result<(), BridgeError> {
    let aliases = index
        .tracked_files()
        .into_iter()
        .filter_map(|tracked| {
            let name = tracked.display().to_string().replace('\\', "/");
            (name != path && name.eq_ignore_ascii_case(path)).then_some(name)
        })
        .collect::<Vec<_>>();
    if aliases.is_empty() {
        return Ok(());
    }
    let refused = BridgeError::invalid_params(format!(
        "vcs.stage: ambiguous case variant of {path}; refusing to change another index entry"
    ));
    let Ok(workdir) = util::try_working_dir() else {
        return Err(refused);
    };
    if !crate::utils::path_case::probe_dir_ignore_case(&workdir) {
        return Err(refused);
    }
    let root = crate::utils::beneath::open_root(&workdir).map_err(|_| {
        BridgeError::invalid_params(format!(
            "vcs.stage: ambiguous case variant of {path}; refusing to change another index entry"
        ))
    })?;
    for alias in &aliases {
        if crate::utils::beneath::same_nofollow_entry(&root, Path::new(path), Path::new(alias)).ok()
            != Some(true)
        {
            return Err(BridgeError::invalid_params(format!(
                "vcs.stage: ambiguous case variant of {path}; refusing to change another index entry"
            )));
        }
    }
    for alias in aliases {
        for stage in 0..4 {
            index.remove(&alias, stage);
        }
    }
    Ok(())
}

/// Use the directory entry's own spelling when the worktree folds case.
///
/// The walk lists only a directory opened beneath the worktree with
/// `O_NOFOLLOW`. A symlink ancestor is refused before its target is listed, so
/// an outside name cannot be adopted. On a case-sensitive filesystem the
/// caller's spelling is kept and a different-case neighbor is never rewritten.
fn on_disk_spelling(path: &str) -> Result<String, BridgeError> {
    // Refuse a symlink ancestor before any directory listing. The returned
    // spelling is still discovered below; this call does not rewrite it.
    super::vcs::canonicalize_existing_spelling(path)?;
    let Ok(workdir) = util::try_working_dir() else {
        return Ok(path.to_string());
    };
    let fold = crate::utils::path_case::probe_dir_ignore_case(&workdir);
    let root = crate::utils::beneath::open_root(&workdir).map_err(|error| {
        BridgeError::internal(format!(
            "vcs path '{path}' could not pin the worktree ({})",
            super::vcs::withheld_detail(&error)
        ))
    })?;
    let components: Vec<&str> = path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect();
    let mut resolved = Vec::with_capacity(components.len());
    for (index, component) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        let parent = resolved.join("/");
        if !fold {
            let candidate = if parent.is_empty() {
                (*component).to_string()
            } else {
                format!("{parent}/{component}")
            };
            match crate::utils::beneath::lstat_beneath(&root, Path::new(&candidate)) {
                Ok(stat) if stat.is_symlink && !last => {
                    return Err(BridgeError::invalid_params(format!(
                        "vcs path '{path}' descends through symlink '{candidate}'; refusing to follow it"
                    )));
                }
                Ok(stat) if !stat.is_dir && !last => {
                    return Err(BridgeError::invalid_params(format!(
                        "vcs path '{path}' descends through non-directory '{candidate}'"
                    )));
                }
                Ok(_) => {
                    resolved.push((*component).to_string());
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    resolved.extend(components[index..].iter().map(|part| (*part).to_string()));
                    break;
                }
                Err(error) => {
                    return Err(BridgeError::internal(format!(
                        "vcs path '{path}' could not be inspected ({})",
                        super::vcs::withheld_detail(&error)
                    )));
                }
            }
        }
        let directory =
            crate::utils::beneath::open_beneath(&root, Path::new(&parent)).map_err(|error| {
                BridgeError::internal(format!(
                    "vcs path '{path}' could not open a repository directory ({})",
                    super::vcs::withheld_detail(&error)
                ))
            })?;
        let listing = crate::utils::beneath::read_dir_fd(directory).map_err(|error| {
            BridgeError::internal(format!(
                "vcs path '{path}' could not list a repository directory ({})",
                super::vcs::withheld_detail(&error)
            ))
        })?;
        let mut exact = None;
        let mut folded = None;
        let mut seen = 0usize;
        for entry in listing {
            let entry = entry.map_err(|error| {
                BridgeError::internal(format!(
                    "vcs path '{path}' could not list a repository directory ({})",
                    super::vcs::withheld_detail(&error)
                ))
            })?;
            seen += 1;
            if seen > MAX_CASE_DIR_ENTRIES {
                return Err(BridgeError::invalid_params(format!(
                    "vcs path '{path}' requires listing a directory above the {MAX_CASE_DIR_ENTRIES}-entry cap"
                )));
            }
            let Some(name) = entry.name.to_str() else {
                continue;
            };
            if name == *component {
                exact = Some(name.to_string());
                break;
            }
            if name.eq_ignore_ascii_case(component) {
                if folded.is_some() {
                    return Err(BridgeError::invalid_params(format!(
                        "vcs path '{path}' matches more than one directory entry at '{component}'"
                    )));
                }
                folded = Some(name.to_string());
            }
        }
        let chosen = if let Some(name) = exact {
            name
        } else if let Some(name) = folded {
            name
        } else {
            resolved.extend(components[index..].iter().map(|part| (*part).to_string()));
            break;
        };
        let relative = if parent.is_empty() {
            chosen.clone()
        } else {
            format!("{parent}/{chosen}")
        };
        let stat =
            crate::utils::beneath::lstat_beneath(&root, Path::new(&relative)).map_err(|error| {
                BridgeError::internal(format!(
                    "vcs path '{path}' could not inspect '{component}' ({})",
                    super::vcs::withheld_detail(&error)
                ))
            })?;
        if stat.is_symlink && !last {
            return Err(BridgeError::invalid_params(format!(
                "vcs path '{path}' descends through symlink '{relative}'; refusing to follow it"
            )));
        }
        if !last && !stat.is_dir {
            return Err(BridgeError::invalid_params(format!(
                "vcs path '{path}' descends through non-directory '{relative}'"
            )));
        }
        resolved.push(chosen);
    }
    Ok(resolved.join("/"))
}

/// Serialize the index away from the live index so a serialization or write
/// failure cannot truncate it. This does not serialize with other index writers.
fn serialize_index(index: &git_internal::internal::index::Index) -> Result<Vec<u8>, BridgeError> {
    let index_path = path::try_index()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: locate index", error))?;
    let parent = index_path
        .parent()
        .ok_or_else(|| BridgeError::internal("vcs.stage: index has no parent directory"))?;
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs.stage: create temporary index", error)
    })?;
    index
        .save(temporary.path())
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: serialize index", error))?;
    read_index_bounded(temporary.path())
}

fn stage_tracked_deletion(
    path: &str,
    index: &mut git_internal::internal::index::Index,
) -> Result<(), BridgeError> {
    remove_case_aliases(index, path)?;
    let mut removed = false;
    for stage in 0..4 {
        if index.remove(path, stage).is_some() {
            removed = true;
        }
    }
    if !removed {
        return Err(BridgeError::invalid_params(format!(
            "vcs.stage: {path} is not tracked, so its absence is not a deletion"
        )));
    }
    Ok(())
}

fn read_optional_worktree_bytes(relative: &str) -> Result<Option<(Vec<u8>, u32)>, BridgeError> {
    let workdir = util::try_working_dir()
        .map_err(|_| BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"))?;
    let root = crate::utils::beneath::open_root(&workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: pin root", error))?;
    match crate::utils::beneath::open_file_beneath_nonblocking(&root, Path::new(relative)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error)
            if error.kind() == std::io::ErrorKind::InvalidInput
                || error.kind() == std::io::ErrorKind::IsADirectory =>
        {
            Err(BridgeError::invalid_params(format!(
                "vcs.stage: {relative} is not a regular file or symlink"
            )))
        }
        Err(error) => Err(vcs::internal_hiding_host_paths(
            &format!("vcs.stage: open {relative}"),
            error,
        )),
        Ok(file) => Ok(Some(read_limited(file, relative)?)),
    }
}

#[cfg(test)]
fn read_bytes_beneath(root: &std::fs::File, relative: &str) -> Result<Vec<u8>, BridgeError> {
    let file = crate::utils::beneath::open_file_beneath_nonblocking(root, Path::new(relative))
        .map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs.stage: open {relative}"), error)
        })?;
    read_limited(file, relative).map(|(bytes, _mode)| bytes)
}

fn read_limited(file: std::fs::File, relative: &str) -> Result<(Vec<u8>, u32), BridgeError> {
    use std::io::Read;
    let mode = index_mode_from_file(&file);
    let mut bytes = Vec::new();
    file.take(MAX_STAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs.stage: read {relative}"), error)
        })?;
    if bytes.len() as u64 > MAX_STAGE_BYTES {
        return Err(BridgeError::invalid_params(format!(
            "vcs.stage: oversized path {relative}"
        )));
    }
    Ok((bytes, mode))
}

fn index_mode_from_file(file: &std::fs::File) -> u32 {
    let Ok(metadata) = file.metadata() else {
        return 0o100644;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.file_type().is_symlink() {
            return 0o120000;
        }
        if metadata.permissions().mode() & 0o111 != 0 {
            return 0o100755;
        }
    }
    let _ = metadata;
    0o100644
}

fn quiet_output() -> crate::utils::output::OutputConfig {
    let mut output = vcs::service_output();
    output.json_format = None;
    output
}

fn confirm_workdir_root(
    workdir: &Path,
    expected: &crate::utils::beneath::EntryIdentity,
) -> Result<(), BridgeError> {
    let current = crate::utils::beneath::open_root(workdir).map_err(|error| {
        vcs::stale_fence_hiding_host_paths("vcs fence: cannot reopen workdir root", error)
    })?;
    let actual = crate::utils::beneath::entry_identity_beneath(&current, Path::new("")).map_err(
        |error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot identify workdir root", error)
        },
    )?;
    if &actual != expected {
        return Err(BridgeError::stale_fence(
            "vcs fence: workdir root changed during observation",
        ));
    }
    Ok(())
}

async fn worktree_generation(workdir: &Path) -> Result<String, BridgeError> {
    let root = crate::utils::beneath::open_root(workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: cannot pin root", error))?;
    let identity =
        crate::utils::beneath::entry_identity_beneath(&root, Path::new("")).map_err(|error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot identify workdir root", error)
        })?;
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: read index", error))?;
    let unstaged = changes_to_be_staged()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: read worktree", error))?;
    confirm_workdir_root(workdir, &identity)?;
    let total = change_count(&staged).saturating_add(change_count(&unstaged));
    if total > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    let mut rows = BTreeSet::new();
    let mut remaining = 32 * 1024 * 1024u64;
    collect_rows(&root, &staged, "staged", &mut rows, &mut remaining)?;
    collect_rows(&root, &unstaged, "unstaged", &mut rows, &mut remaining)?;
    confirm_workdir_root(workdir, &identity)?;
    if rows.len() > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    Ok(sha256_hex(
        rows.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
    ))
}

fn change_count(changes: &crate::command::status::Changes) -> usize {
    changes
        .new
        .len()
        .saturating_add(changes.modified.len())
        .saturating_add(changes.deleted.len())
        .saturating_add(changes.renamed.len())
}

fn collect_rows(
    root: &std::fs::File,
    changes: &crate::command::status::Changes,
    state: &str,
    rows: &mut BTreeSet<String>,
    remaining: &mut u64,
) -> Result<(), BridgeError> {
    if change_count(changes) > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    for path in changes
        .new
        .iter()
        .chain(changes.modified.iter())
        .chain(changes.deleted.iter())
    {
        let rel = path.display().to_string().replace('\\', "/");
        rows.insert(format!(
            "{state}\t{rel}\t{}",
            content_digest(root, &rel, remaining)?
        ));
    }
    for (old, new) in &changes.renamed {
        let rel = new.display().to_string().replace('\\', "/");
        let from = old.display().to_string().replace('\\', "/");
        rows.insert(format!(
            "rename\t{from}\t{rel}\t{}",
            content_digest(root, &rel, remaining)?
        ));
    }
    Ok(())
}

fn content_digest(
    root: &std::fs::File,
    relative: &str,
    remaining: &mut u64,
) -> Result<String, BridgeError> {
    use std::io::Read;
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let relative_path = Path::new(relative);
    let metadata = match crate::utils::beneath::lstat_beneath(root, relative_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match crate::utils::beneath::lstat_beneath(root, relative_path) {
                Err(again) if again.kind() == std::io::ErrorKind::NotFound => Ok("missing".into()),
                Ok(_) => Err(BridgeError::stale_fence(format!(
                    "vcs fence: {relative} appeared during absence check"
                ))),
                Err(again) => Err(BridgeError::internal(format!(
                    "vcs fence: cannot recheck missing {relative}: {}",
                    super::vcs::withheld_detail(&again)
                ))),
            };
        }
        Err(error) => {
            return Err(vcs::internal_hiding_host_paths(
                &format!("vcs fence: cannot inspect {relative}"),
                error,
            ));
        }
    };
    let before_identity = crate::utils::beneath::entry_identity_beneath(root, relative_path)
        .map_err(|error| {
            vcs::stale_fence_hiding_host_paths(
                &format!("vcs fence: cannot identify {relative}"),
                error,
            )
        })?;
    let mode = metadata.mode;
    if metadata.is_symlink {
        let target =
            crate::utils::beneath::read_symlink_beneath(root, relative_path).map_err(|error| {
                vcs::internal_hiding_host_paths(
                    &format!("vcs fence: cannot read symlink {relative}"),
                    error,
                )
            })?;
        let after_identity = crate::utils::beneath::entry_identity_beneath(root, relative_path)
            .map_err(|error| {
                vcs::stale_fence_hiding_host_paths(
                    &format!("vcs fence: cannot reidentify {relative}"),
                    error,
                )
            })?;
        let after_metadata =
            crate::utils::beneath::lstat_beneath(root, relative_path).map_err(|error| {
                vcs::stale_fence_hiding_host_paths(
                    &format!("vcs fence: cannot restat link {relative}"),
                    error,
                )
            })?;
        let target_after = crate::utils::beneath::read_symlink_beneath(root, relative_path)
            .map_err(|error| {
                vcs::stale_fence_hiding_host_paths(
                    &format!("vcs fence: cannot reread link {relative}"),
                    error,
                )
            })?;
        if before_identity != after_identity
            || !after_metadata.is_symlink
            || metadata.mode != after_metadata.mode
            || metadata.ctime_sec != after_metadata.ctime_sec
            || metadata.ctime_nsec != after_metadata.ctime_nsec
            || target != target_after
        {
            return Err(BridgeError::stale_fence(format!(
                "vcs fence: {relative} changed while reading link"
            )));
        }
        *remaining = remaining.checked_sub(target.len() as u64).ok_or_else(|| {
            BridgeError::invalid_params("vcs fence aggregate byte budget exceeded")
        })?;
        return Ok(format!("symlink:{mode}:{}", sha256_hex(&target)));
    }
    if !metadata.is_file || metadata.len > MAX_BYTES {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence: unsupported or oversized path {relative}"
        )));
    }
    let file = crate::utils::beneath::open_file_beneath_nonblocking(root, relative_path).map_err(
        |error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot open {relative}"), error)
        },
    )?;
    let opened_identity = crate::utils::beneath::identity_for_file(&file).map_err(|error| {
        vcs::stale_fence_hiding_host_paths(
            &format!("vcs fence: cannot identify open {relative}"),
            error,
        )
    })?;
    if before_identity != opened_identity {
        return Err(BridgeError::stale_fence(format!(
            "vcs fence: {relative} replaced before open"
        )));
    }
    let opened =
        crate::utils::beneath::RawLstat::from_metadata(&file.metadata().map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot stat {relative}"), error)
        })?)
        .map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot inspect {relative}"), error)
        })?;
    if !opened.is_file
        || opened.mode != metadata.mode
        || opened.len != metadata.len
        || opened.mtime_sec != metadata.mtime_sec
        || opened.mtime_nsec != metadata.mtime_nsec
        || opened.ctime_sec != metadata.ctime_sec
        || opened.ctime_nsec != metadata.ctime_nsec
    {
        return Err(BridgeError::stale_fence(format!(
            "vcs fence: {relative} changed before reading"
        )));
    }
    let available = MAX_BYTES.min(*remaining);
    let mut bytes = Vec::new();
    (&file)
        .take(available + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot read {relative}"), error)
        })?;
    if bytes.len() as u64 > available {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence: oversized path {relative}"
        )));
    }
    let after =
        crate::utils::beneath::RawLstat::from_metadata(&file.metadata().map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot restat {relative}"), error)
        })?)
        .map_err(|error| {
            vcs::internal_hiding_host_paths(&format!("vcs fence: cannot inspect {relative}"), error)
        })?;
    if after.mode != opened.mode
        || after.len != opened.len
        || after.mtime_sec != opened.mtime_sec
        || after.mtime_nsec != opened.mtime_nsec
        || after.ctime_sec != opened.ctime_sec
        || after.ctime_nsec != opened.ctime_nsec
    {
        return Err(BridgeError::stale_fence(format!(
            "vcs fence: {relative} changed while reading"
        )));
    }
    let after_identity = crate::utils::beneath::entry_identity_beneath(root, relative_path)
        .map_err(|error| {
            vcs::stale_fence_hiding_host_paths(
                &format!("vcs fence: cannot reidentify {relative}"),
                error,
            )
        })?;
    if opened_identity != after_identity {
        return Err(BridgeError::stale_fence(format!(
            "vcs fence: {relative} replaced while reading"
        )));
    }
    *remaining -= bytes.len() as u64;
    Ok(format!("file:{mode}:{}", sha256_hex(&bytes)))
}

#[cfg(all(test, unix))]
mod fence_tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn stage_snapshot_refuses_an_oversized_file() {
        let dir = tempfile::tempdir().expect("fixture");
        let file = std::fs::File::create(dir.path().join("big")).expect("create");
        file.set_len(16 * 1024 * 1024 + 1).expect("sparse size");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let error = read_bytes_beneath(&root, "big").expect_err("oversized");
        assert!(error.message.contains("oversized"), "{}", error.message);
    }

    #[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
    #[test]
    fn fault_hook_cannot_remove_a_file_without_test_feature() {
        let dir = tempfile::tempdir().expect("fixture");
        let path = dir.path().join("keep");
        std::fs::write(&path, b"keep").expect("write");
        remove_during_publish();
        assert_eq!(std::fs::read(path).expect("file remains"), b"keep");
    }

    #[test]
    fn case_alias_does_not_delete_a_distinct_index_entry() {
        use git_internal::internal::{
            index::{Index, IndexEntry},
            object::types::ObjectType,
        };
        let mut index = Index::new();
        let oid = git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, b"first");
        index.add(IndexEntry::new_from_blob("Name".into(), oid, 5));
        assert!(remove_case_aliases(&mut index, "name").is_err());
        assert!(index.get("Name", 0).is_some(), "distinct entry was removed");
    }

    #[test]
    fn path_limit_rejects_before_opening_any_path() {
        let changes = crate::command::status::Changes {
            new: (0..=MAX_FENCE_PATHS)
                .map(|n| format!("missing-{n}").into())
                .collect(),
            ..Default::default()
        };
        let mut rows = BTreeSet::new();
        let fixture = tempfile::tempdir().expect("fixture");
        let root = crate::utils::beneath::open_root(fixture.path()).expect("root");
        let result = collect_rows(&root, &changes, "unstaged", &mut rows, &mut 32_000_000);
        let error = result.expect_err("count must be refused");
        assert!(error.message.contains("more than"), "{}", error.message);
        assert!(rows.is_empty(), "must reject count before hashing");
    }

    #[test]
    fn fence_rejects_symlinked_ancestor_even_when_target_is_inside_root() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::create_dir(dir.path().join("real")).expect("directory");
        std::fs::write(dir.path().join("real/file"), b"data").expect("file");
        symlink("real", dir.path().join("alias")).expect("symlink");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        assert!(content_digest(&root, "alias/file", &mut 100).is_err());
    }

    #[test]
    fn replacement_changes_path_identity_while_open_file_stays_old() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("item"), b"first").expect("first");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let opened = crate::utils::beneath::open_file_beneath_nonblocking(&root, Path::new("item"))
            .expect("open");
        let before = crate::utils::beneath::identity_for_file(&opened).expect("identity");
        std::fs::rename(dir.path().join("item"), dir.path().join("old")).expect("rename");
        std::fs::write(dir.path().join("item"), b"second").expect("replacement");
        let after = crate::utils::beneath::entry_identity_beneath(&root, Path::new("item"))
            .expect("identity");
        assert_ne!(before, after);
    }

    #[test]
    fn replaced_workdir_root_is_rejected() {
        let parent = tempfile::tempdir().expect("parent");
        let original = parent.path().join("repo");
        std::fs::create_dir(&original).expect("repo");
        let root = crate::utils::beneath::open_root(&original).expect("root");
        let identity =
            crate::utils::beneath::entry_identity_beneath(&root, Path::new("")).expect("identity");
        let moved = parent.path().join("moved");
        std::fs::rename(&original, &moved).expect("rename");
        std::fs::create_dir(&original).expect("replacement");
        let error = confirm_workdir_root(&original, &identity).expect_err("replaced root");
        assert!(error.message.contains("root"), "{}", error.message);
    }

    #[test]
    fn stable_absence_is_recorded_without_reading_a_file() {
        let dir = tempfile::tempdir().expect("fixture");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        assert_eq!(
            content_digest(&root, "missing", &mut 100).expect("absence"),
            "missing"
        );
    }

    #[test]
    fn index_missing_and_symlink_fail_closed() {
        let dir = tempfile::tempdir().expect("fixture");
        let index = dir.path().join("index");
        assert!(read_index_bounded(&index).is_err());
        std::fs::write(dir.path().join("target"), b"private").expect("target");
        symlink("target", &index).expect("symlink");
        assert!(read_index_bounded(&index).is_err());
        std::fs::remove_file(&index).expect("remove fixture symlink");
        std::fs::write(&index, b"index bytes").expect("regular index");
        assert_eq!(
            read_index_bounded(&index).expect("regular index"),
            b"index bytes"
        );
    }

    #[test]
    fn special_file_is_refused_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let dir = tempfile::tempdir().expect("fixture");
        let fifo = dir.path().join("pipe");
        let name = CString::new(fifo.as_os_str().as_bytes()).expect("path");
        // SAFETY: name is a NUL-terminated path and mkfifo does not retain it.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let started = std::time::Instant::now();
        let error = crate::utils::beneath::open_file_beneath_nonblocking(&root, Path::new("pipe"))
            .expect_err("FIFO must not be read as a regular file");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn pinned_root_does_not_switch_when_path_is_replaced() {
        let parent = tempfile::tempdir().expect("parent");
        let original = parent.path().join("root");
        let moved = parent.path().join("moved");
        std::fs::create_dir(&original).expect("create root");
        std::fs::write(original.join("file"), b"original").expect("write");
        let root = crate::utils::beneath::open_root(&original).expect("root");
        std::fs::rename(&original, &moved).expect("rename");
        std::fs::create_dir(&original).expect("replace root");
        std::fs::write(original.join("file"), b"replacement").expect("write replacement");
        let pinned = content_digest(&root, "file", &mut 100).expect("digest");
        let moved_root = crate::utils::beneath::open_root(&moved).expect("moved root");
        assert_eq!(
            pinned,
            content_digest(&moved_root, "file", &mut 100).expect("same")
        );
        let replacement = crate::utils::beneath::open_root(&original).expect("replacement");
        assert_ne!(
            pinned,
            content_digest(&replacement, "file", &mut 100).expect("different")
        );
    }

    #[test]
    fn aggregate_budget_refuses_file_larger_than_remaining_budget() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("file"), b"12345").expect("write");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let error = content_digest(&root, "file", &mut 4).expect_err("budget");
        assert!(error.message.contains("oversized"));
    }

    #[test]
    fn distinct_symlinks_with_equal_target_contents_have_distinct_fences() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("a"), b"same").expect("write");
        std::fs::write(dir.path().join("b"), b"same").expect("write");
        symlink("a", dir.path().join("link1")).expect("symlink");
        symlink("b", dir.path().join("link2")).expect("symlink");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        assert_ne!(
            content_digest(&root, "link1", &mut 32_000_000).expect("first"),
            content_digest(&root, "link2", &mut 32_000_000).expect("second")
        );
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
