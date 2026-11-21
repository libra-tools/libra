//! `vcs.stage`, `vcs.unstage`, and `vcs.commit` (plan-20261118 LVC-06).
//!
//! These do not reuse `commit.create`. Each call names literal repository
//! paths, an `operation_id`, and the HEAD/index/worktree fence the caller
//! observed. A stale fence returns before any index or commit write.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{protocol::BridgeError, vcs};
use crate::{
    command::status::{changes_to_be_committed_safe, changes_to_be_staged},
    utils::{path, util},
};

const MAX_FENCE_PATHS: usize = 256;
/// The aggregate content budget of one fence, in bytes.
const MAX_FENCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CASE_DIR_ENTRIES: usize = 100_000;
static WRITE_GATE: Mutex<()> = Mutex::const_new(());
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FENCE_RECHECK_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static STAGE_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static STAGE_CAPTURE_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
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
static PRE_PUBLISH_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FENCE_OBSERVATION_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static INDEX_OBSERVATION_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);
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

/// Test seam: change the file before its plan is captured.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_stage_capture_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *STAGE_CAPTURE_REWRITE
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

/// Test seam: rewrite a file after preparing the scratch index and before rechecking the fence.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_pre_publish_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *PRE_PUBLISH_REWRITE
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
fn rewrite_before_stage_plan_capture() {
    let Some(path) = STAGE_CAPTURE_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let _ = std::fs::write(path, b"prepared snapshot differs from admission\n");
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

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub(super) fn rewrite_before_index_publication() {
    let Some(path) = PRE_PUBLISH_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let _ = std::fs::write(path, b"changed after index preparation\n");
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
fn rewrite_before_stage_plan_capture() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
fn rewrite_for_fence_recheck() {}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
pub(super) fn rewrite_before_index_publication() {}

/// Test seam: change the staged path after the preparation confirmed it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static STAGE_CONFIRMED_REWRITE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Test seam: change the staged path after the preparation confirmed it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_stage_confirmed_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *STAGE_CONFIRMED_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Change the staged path after the preparation confirmed it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub(super) fn rewrite_after_stage_confirmation() {
    let Some(path) = STAGE_CONFIRMED_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let _ = std::fs::write(path, b"changed after the preparation confirmed it\n");
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
pub(super) fn rewrite_after_stage_confirmation() {}

/// Test seam: replace a path with a directory between the status scan's
/// classification read and the verification of its observation.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_classification_window_directory_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_classification_window_directory_for_test(path);
}

/// Test seam: recreate a staged deletion before the status walk sees it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_staged_absence_recreate_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_staged_absence_recreate_for_test(path);
}

/// Test seam: change a file between the two fence observations of one read.
///
/// The seam exists so a test can prove that a fence is published only when it
/// describes one stable observation. It is not used by the protocol.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_fence_observation_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *FENCE_OBSERVATION_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Test seam: rewrite a path between the status scan's classification read and
/// the verification of its observation.
///
/// The seam exists so a test can prove that a path which changed while the
/// scan classified it cannot enter the write fence. It is not used by the
/// protocol.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_classification_window_rewrite_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_classification_window_rewrite_for_test(path);
}

/// Test seam: rewrite a path after its second classification hash.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_classification_digest_window_rewrite_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_classification_digest_window_rewrite_for_test(path);
}

/// Test seam: make the content capture of a path fail.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_forced_capture_failure_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_forced_capture_failure_for_test(path);
}

/// Test seam: make the capture of an already observed path fail.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_forced_recapture_failure_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_forced_recapture_failure_for_test(path);
}

/// Test seam: make a path vanish while its identity is captured.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_forced_vanishing_path_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_forced_vanishing_path_for_test(path);
}

/// Test seam: make the status observation root unpinnable.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_force_observation_root_failure_for_test(enabled: bool) {
    crate::command::status::set_force_observation_root_failure_for_test(enabled);
}

/// Test seam: make the worktree metadata read of a path fail.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_forced_worktree_metadata_failure_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_forced_worktree_metadata_failure_for_test(path);
}

/// Test seam: make the metadata capture of a path fail.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_forced_entry_failure_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_forced_entry_failure_for_test(path);
}

/// Test seam: remove a path after the staged capture.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_remove_after_staged_capture_for_test(path: Option<std::path::PathBuf>) {
    crate::command::status::set_remove_after_staged_capture_for_test(path);
}

/// Add a worktree path after the final status scan but before fence hashing.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub(super) fn rewrite_after_final_status_scan() {
    let Some(path) = FENCE_OBSERVATION_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let _ = std::fs::write(path, b"appeared after the final status scan\n");
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
pub(super) fn rewrite_after_final_status_scan() {}

/// Reconcile a changed index after the status observation test seam.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub(super) fn rewrite_after_status_observation() {
    replace_index_after_observation();
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
pub(super) fn rewrite_after_status_observation() {}

/// Test seam: replace the index after the observation read it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_index_observation_rewrite_for_test(path: Option<std::path::PathBuf>) {
    *INDEX_OBSERVATION_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Replace the index after the observation read it.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub(super) fn replace_index_after_observation() {
    let Some(prepared) = INDEX_OBSERVATION_REWRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let Ok(live) = path::try_index() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&prepared) else {
        return;
    };
    let _ = std::fs::write(&live, bytes);
}

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

/// The write fence for an observation the caller already made.
///
/// The change lists, the HEAD and the index generation are the ones the
/// caller read, so the fence describes the same state as the page the caller
/// saw. Reading the repository again here would let the fence describe a
/// different state than the one the caller approved.
pub fn fence_for_observation(
    head: Option<git_internal::hash::ObjectHash>,
    index_generation: String,
    worktree_generation: String,
) -> WriteFence {
    WriteFence {
        head: head.map(|oid| oid.to_string()).unwrap_or_default(),
        index_generation,
        worktree_generation,
    }
}

/// The write fence generation of an observation the caller already made.
///
/// The rows come from the content digests the status scan captured while it
/// classified the paths, so the generation describes exactly the content the
/// scan observed. A later change to that content produces a different
/// generation, and the write refuses the stale fence.
pub(crate) async fn worktree_generation_for_observation(
    workdir: &Path,
    staged: &crate::command::status::Changes,
    unstaged: &crate::command::status::Changes,
    stamps: &crate::command::status::WorktreeStamps,
) -> Result<String, BridgeError> {
    let root = crate::utils::beneath::open_root(workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: cannot pin root", error))?;
    verify_all_observed_stamps(&root, stamps)?;
    let total = change_count(staged).saturating_add(change_count(unstaged));
    if total > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    let mut rows = BTreeSet::new();
    let mut remaining = MAX_FENCE_BYTES;
    let mut charged = BTreeSet::new();
    collect_rows_from_stamps(
        staged,
        "staged",
        &mut rows,
        stamps,
        &mut remaining,
        &mut charged,
    )?;
    collect_rows_from_stamps(
        unstaged,
        "unstaged",
        &mut rows,
        stamps,
        &mut remaining,
        &mut charged,
    )?;
    Ok(sha256_hex(
        rows.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
    ))
}

/// The fence rows of one change list, taken from the digests the scan
/// captured.
///
/// The path count and the byte budget are the same ones the write-side
/// recheck applies, so a published fence is always one a write can consume.
fn collect_rows_from_stamps(
    changes: &crate::command::status::Changes,
    state: &str,
    rows: &mut BTreeSet<String>,
    stamps: &crate::command::status::WorktreeStamps,
    remaining: &mut u64,
    charged: &mut BTreeSet<std::path::PathBuf>,
) -> Result<(), BridgeError> {
    // A path that is both staged and unstaged is charged once: the fence
    // covers its content once, and the repeated row reuses that digest.
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
        // The repository-relative path is used verbatim: rewriting it would
        // hash another file than the one that changed.
        let rel = path.display().to_string();
        let (digest, bytes) = observed_digest(stamps, path)?;
        // A path that is both staged and unstaged is charged once: the fence
        // covers its content once.
        if charged.insert(path.to_path_buf()) {
            *remaining = remaining.checked_sub(bytes).ok_or_else(|| {
                BridgeError::invalid_params("vcs fence aggregate byte budget exceeded")
            })?;
        }
        rows.insert(format!("{state}\t{rel}\t{digest}"));
    }
    for (old, new) in &changes.renamed {
        let rel = new.display().to_string();
        let from = old.display().to_string();
        let (digest, bytes) = observed_digest(stamps, new)?;
        if charged.insert(new.to_path_buf()) {
            *remaining = remaining.checked_sub(bytes).ok_or_else(|| {
                BridgeError::invalid_params("vcs fence aggregate byte budget exceeded")
            })?;
        }
        rows.insert(format!("rename\t{from}\t{rel}\t{digest}"));
    }
    Ok(())
}

/// The content digest the status scan captured for one path, with the bytes
/// it covers.
fn observed_digest(
    stamps: &crate::command::status::WorktreeStamps,
    path: &Path,
) -> Result<(String, u64), BridgeError> {
    let Some(observed) = stamps.get(path) else {
        return Err(BridgeError::stale_fence(format!(
            "vcs fence: {} was not captured by the status scan",
            path.display()
        )));
    };
    match observed {
        crate::command::status::ObservedPath::Absent => Ok(("missing".to_string(), 0)),
        crate::command::status::ObservedPath::Observed(stamp) => match &stamp.digest {
            Some(digest) => Ok((digest.clone(), stamp.bytes)),
            None => Err(BridgeError::stale_fence(format!(
                "vcs fence: {} carries no captured content",
                path.display()
            ))),
        },
        crate::command::status::ObservedPath::Unstable => Err(BridgeError::stale_fence(format!(
            "vcs fence: {} changed while the status scan read it",
            path.display()
        ))),
    }
}

/// The index bytes of one snapshot, read with the fence's own bounds.
pub(crate) fn read_index_snapshot_bytes(index: &Path) -> Result<Vec<u8>, BridgeError> {
    read_index_bounded(index)
}

/// The generation of an index snapshot.
pub(crate) fn index_generation_of(bytes: &[u8]) -> String {
    sha256_hex(bytes)
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
    /// The source snapshots whose bytes produced the prepared index.
    /// These are rechecked at the final publication boundary.
    stage_plans: Vec<StagePlan>,
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

    /// Check that the source snapshots whose bytes produced this index are
    /// still current.
    ///
    /// This runs at the publication boundary, immediately before the index is
    /// replaced. A refusal here is definite: the index was not replaced, so
    /// the caller records a terminal refusal instead of an uncertain
    /// publication.
    pub fn verify_prepared_sources(&self) -> Result<(), BridgeError> {
        for plan in &self.stage_plans {
            confirm_prepared_stage_plan(plan)?;
        }
        Ok(())
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
    ///
    /// An error here leaves the publication uncertain: the replacement may
    /// have happened. Only a refusal raised before the replacement starts is
    /// definite, and it is raised by `verify_prepared_sources` instead.
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

fn staged_path_names(staged: &crate::command::status::Changes) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for path in staged
        .new
        .iter()
        .chain(staged.modified.iter())
        .chain(staged.deleted.iter())
    {
        // Each index change remains a separate authorization entry, even when
        // the worktree filesystem resolves two spellings to the same file.
        paths.insert(path.display().to_string());
    }
    for (_old, new) in &staged.renamed {
        paths.insert(new.display().to_string());
    }
    paths
}

/// The sole case-folding exception is a content-proven 1:1 rename whose
/// source no longer exists in the index and whose two path spellings resolve
/// to the same worktree entry. All other staged paths stay exact.
async fn case_only_rename_sources_to_omit(
    staged_paths: &BTreeSet<String>,
) -> Result<BTreeSet<String>, BridgeError> {
    let paths: Vec<&str> = staged_paths.iter().map(String::as_str).collect();
    let has_case_only_pair = paths.iter().enumerate().any(|(index, left)| {
        paths
            .iter()
            .skip(index + 1)
            .any(|right| left != right && left.eq_ignore_ascii_case(right))
    });
    if !has_case_only_pair {
        return Ok(BTreeSet::new());
    }

    let workdir = util::try_working_dir()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: locate worktree", error))?;
    if !crate::utils::path_case::probe_dir_ignore_case(&workdir) {
        return Ok(BTreeSet::new());
    }
    let root = crate::utils::beneath::open_root(&workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: pin worktree", error))?;
    let index_path = path::try_index()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: locate index", error))?;
    let index = git_internal::internal::index::Index::load(&index_path)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: load index", error))?;
    let head_entries = head_tree_entries().await?;
    let renames = proven_staged_renames(&head_entries, &index).await?;
    let mut sources_to_omit = BTreeSet::new();

    for rename in renames {
        let [source] = rename.sources.as_slice() else {
            continue;
        };
        let [destination] = rename.destinations.as_slice() else {
            continue;
        };
        if source == destination
            || !source.eq_ignore_ascii_case(destination)
            || !staged_paths.contains(source)
            || !staged_paths.contains(destination)
        {
            continue;
        }

        // A differently-cased index entry is separate authorization scope,
        // even when the worktree resolves it to the same directory entry.
        let aliases: Vec<(u8, String)> = (0..4)
            .flat_map(|stage| {
                index
                    .tracked_entries(stage)
                    .into_iter()
                    .map(move |entry| (stage, entry.name.clone()))
            })
            .filter(|(_, name)| name.eq_ignore_ascii_case(destination))
            .collect();
        if aliases.len() != 1 || aliases[0].0 != 0 || aliases[0].1 != *destination {
            continue;
        }
        if crate::utils::beneath::same_nofollow_entry(
            &root,
            Path::new(source),
            Path::new(destination),
        )
        .ok()
            != Some(true)
        {
            continue;
        }
        sources_to_omit.insert(source.clone());
    }
    Ok(sources_to_omit)
}

fn require_exact_commit_scope(
    staged_paths: &BTreeSet<String>,
    requested_paths: &BTreeSet<String>,
) -> Result<(), BridgeError> {
    if staged_paths != requested_paths {
        return Err(BridgeError::invalid_params(format!(
            "vcs.commit paths {requested_paths:?} do not match the staged set {staged_paths:?}; refusing before creating a commit"
        )));
    }
    Ok(())
}

pub async fn commit_index(
    message: &str,
    requested: &[String],
) -> Result<serde_json::Value, BridgeError> {
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit: read index", error))?;
    // The staged paths are repository-relative already. They are NOT converted
    // to the process directory: the request names repository-relative paths, so
    // converting one side and not the other would compare two different path
    // bases and reject an authorized commit.
    let mut staged_paths = staged_path_names(&staged);
    for source in case_only_rename_sources_to_omit(&staged_paths).await? {
        staged_paths.remove(&source);
    }
    let requested_paths: BTreeSet<String> = requested.iter().cloned().collect();
    require_exact_commit_scope(&staged_paths, &requested_paths)?;
    let created = vcs::commit_create_unguarded(message, false, false).await?;
    Ok(serde_json::json!({
        "commitOid": created["commit"],
        "paths": staged_paths.into_iter().collect::<Vec<_>>(),
    }))
}

/// One refusal that names every path that failed.
///
/// The operation stays all-or-nothing: nothing is published when any path
/// fails. The refusal identifies each failing path with its own reason, so a
/// caller learns every problem from one request instead of one request per
/// path.
fn per_path_refusal(context: &str, failures: &[(String, BridgeError)]) -> BridgeError {
    let Some((_, first)) = failures.first() else {
        return BridgeError::internal(format!("{context}: no failure was recorded"));
    };
    let details = failures
        .iter()
        .map(|(path, error)| format!("{path} ({})", error.message))
        .collect::<Vec<_>>()
        .join("; ");
    BridgeError {
        message: format!(
            "{context}: {} of the selected paths failed: {details}",
            failures.len()
        ),
        ..first.clone()
    }
}

async fn apply_each(paths: &[String], stage: bool) -> Result<PreparedIndexWrite, BridgeError> {
    let mut applied = Vec::new();
    let mut failed: Vec<(String, BridgeError)> = Vec::new();
    let index_path = path::try_index().map_err(|error| {
        vcs::internal_hiding_host_paths("vcs write: cannot locate index snapshot", error)
    })?;
    let index_snapshot = read_index_bounded(&index_path)?;
    if stage {
        let mut index = git_internal::internal::index::Index::load(&index_path)
            .map_err(|error| vcs::internal_hiding_host_paths("vcs.stage: load index", error))?;
        if read_index_bounded(&index_path)? != index_snapshot {
            return Err(BridgeError::stale_fence(
                "stale_state: index changed while preparing stage",
            ));
        }
        let mut refusals: Vec<(String, BridgeError)> = Vec::new();
        let mut failed_paths: BTreeSet<String> = BTreeSet::new();
        let targets = resolve_stage_targets(&index, paths, &mut refusals, &mut failed_paths);
        // Every selected path is checked before anything is stored, and every
        // refusal is reported: one request names each failing path. A path that
        // already failed is not checked again, so it is reported once.
        for path in &targets {
            if failed_paths.contains(path) {
                continue;
            }
            note_stage_attempt();
            if let Err(error) = preflight_stage_path(path) {
                refusals.push((path.clone(), error));
                failed_paths.insert(path.clone());
            }
        }
        break_at_publish();
        for path in &targets {
            if failed_paths.contains(path) {
                continue;
            }
            if let Err(error) = preflight_stage_path(path) {
                refusals.push((path.clone(), error));
                failed_paths.insert(path.clone());
            }
        }
        let mut plans = Vec::with_capacity(targets.len());
        rewrite_before_stage_plan_capture();
        for path in &targets {
            if failed_paths.contains(path) {
                continue;
            }
            match capture_stage_plan(path) {
                Ok(plan) => plans.push(plan),
                Err(error) => {
                    refusals.push((path.clone(), error));
                    failed_paths.insert(path.clone());
                }
            }
        }
        break_on_first_publish();
        for plan in &plans {
            if let Err(error) = confirm_stage_plan(plan) {
                refusals.push((plan.path().to_string(), error));
                failed_paths.insert(plan.path().to_string());
            }
        }
        rewrite_after_stage_confirmation();
        rewrite_before_mode_stat();
        rewrite_during_publish();
        remove_during_publish();
        if read_index_bounded(&index_path)? != index_snapshot {
            return Err(BridgeError::stale_fence(
                "stale_state: index changed while preparing stage",
            ));
        }
        // Every selected path is resolved before anything is stored, so one
        // refused path cannot leave another path's blob in the object store.
        // Every refusal is collected, so one request names each failing path.
        resolve_stage_conversions(&mut plans, &index, &mut refusals, &mut failed_paths);
        for plan in &plans {
            if failed_paths.contains(plan.path()) {
                continue;
            }
            if crate::utils::ignore::should_ignore(
                Path::new(plan.path()),
                crate::utils::ignore::IgnorePolicy::Respect,
                &index,
            ) {
                refusals.push((
                    plan.path().to_string(),
                    BridgeError::invalid_params(format!(
                        "vcs.stage: {} is ignored; refusing to stage it without force",
                        plan.path()
                    )),
                ));
                failed_paths.insert(plan.path().to_string());
            }
        }
        // The case-alias check can refuse a path, so it runs here as well: a
        // batch must learn every refusal before it publishes anything.
        for plan in &plans {
            if failed_paths.contains(plan.path()) {
                continue;
            }
            if let Err(error) = case_alias_to_remove(&index, plan.path()).await {
                refusals.push((plan.path().to_string(), error));
                failed_paths.insert(plan.path().to_string());
            }
        }
        if !refusals.is_empty() {
            return Err(per_path_refusal("vcs.stage", &refusals));
        }
        // The prepared content must still be the content the repository holds:
        // a path that changed while the batch was prepared would otherwise be
        // published with the prepared bytes instead of the authorized ones.
        for plan in &plans {
            if let Err(error) = confirm_stage_plan(plan) {
                return Err(BridgeError::stale_fence(format!(
                    "stale_state: {} changed while the batch was prepared; no index write was \
                     performed ({})",
                    plan.path(),
                    error.message
                )));
            }
        }
        for plan in &plans {
            reject_second_publish()?;
            let path = plan.path().to_string();
            // A per-path refusal here is collected like the earlier ones: the
            // live index is replaced only after every path succeeded, so the
            // batch stays all-or-nothing.
            match publish_stage_plan(plan, &mut index).await {
                Ok(()) => applied.push(path),
                Err(error) => {
                    refusals.push((path.clone(), error));
                    failed_paths.insert(path);
                }
            }
        }
        if !refusals.is_empty() {
            return Err(per_path_refusal("vcs.stage", &refusals));
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
            stage_plans: plans,
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
    // The scratch index is updated by EXACT entry name. A pathspec would
    // match every tracked path under a requested directory and would resolve
    // the request against the process directory; neither is the authorized
    // literal-path scope, so neither is used here.
    let mut scratch_index = git_internal::internal::index::Index::load(scratch.path())
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.unstage: load index", error))?;
    let mut head_entries: Option<
        std::collections::HashMap<
            String,
            (
                git_internal::internal::object::tree::TreeItemMode,
                git_internal::hash::ObjectHash,
            ),
        >,
    > = None;
    let mut renames: Option<Vec<RenameGroup>> = None;
    // Two spellings that differ only in case name the same file only on a
    // case-insensitive filesystem; there the repository's own spelling is
    // restored, and never another entry's.
    let case_insensitive = util::try_working_dir()
        .ok()
        .is_some_and(|workdir| crate::utils::path_case::probe_dir_ignore_case(&workdir));
    for selected in paths {
        match unstage_exact_entry(
            &mut scratch_index,
            selected,
            &mut head_entries,
            &mut renames,
            paths,
            case_insensitive,
        )
        .await
        {
            Ok(()) => applied.push(selected.clone()),
            Err(error) => {
                failed.push((selected.clone(), error));
            }
        }
    }
    if !failed.is_empty() {
        // Nothing is published: the operation stays all-or-nothing, and the
        // refusal names every path that failed.
        return Err(per_path_refusal("vcs.unstage", &failed));
    }
    if applied.is_empty() {
        return Err(BridgeError::internal("vcs write applied no paths"));
    }
    scratch_index
        .save(scratch.path())
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.unstage: save index", error))?;
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
        stage_plans: Vec::new(),
    })
}

/// The index entry names that differ from `name` only in case.
fn case_variant_names(index: &git_internal::internal::index::Index, name: &str) -> Vec<String> {
    (0..4)
        .flat_map(|stage| index.tracked_entries(stage))
        .map(|entry| entry.name.clone())
        .filter(|entry| entry != name && entry.eq_ignore_ascii_case(name))
        .collect()
}

/// Resolve the index entry each staged path names.
///
/// A request authorizes a repository-relative path, and the index stores that
/// path under one exact name. The bridge changes exactly the entry it can
/// prove belongs to the requested file:
///
/// - the index already holds an entry with the requested name: that entry is
///   the one that changes;
/// - the request names the file's own spelling and the index holds exactly
///   one entry that differs only in case: the request is a case-only rename
///   or a case-only spelling of the same file. The old entry is dropped only
///   when the index proves it is an unchanged HEAD spelling of that same
///   file, which `remove_case_aliases` checks;
/// - the request names a case variant of an entry the index holds under the
///   file's own spelling: the request is refused, because it does not name
///   that entry and the caller can retry with the exact name;
/// - the index holds nothing for the file: it is a new file, and it keeps
///   the worktree's own spelling.
fn resolve_stage_targets(
    index: &git_internal::internal::index::Index,
    requested: &[String],
    refusals: &mut Vec<(String, BridgeError)>,
    failed_paths: &mut BTreeSet<String>,
) -> Vec<String> {
    let mut targets = Vec::with_capacity(requested.len());
    for path in requested {
        if (0..4).any(|stage| index.tracked(path, stage)) {
            // A skip-worktree entry names a sparse-checkout path whose
            // worktree copy may be absent or stale. The status scan does not
            // report it, so no fence covers it, and staging it would publish
            // content the peer never saw.
            if index
                .get(path, 0)
                .is_some_and(|entry| entry.flags.skip_worktree)
            {
                refusals.push((
                    path.clone(),
                    BridgeError::invalid_params(format!(
                        "vcs.stage: {path} is a skip-worktree entry, so its worktree copy is not \
                         covered by the write fence; refusing to stage it"
                    )),
                ));
                failed_paths.insert(path.clone());
                continue;
            }
            // A submodule (gitlink) entry names another repository's commit.
            // The status scan does not report it, so no fence covers it, and
            // staging its absence would remove a submodule the peer never saw
            // as changed.
            if crate::command::status::output::is_gitlink_index_entry(index, path) {
                refusals.push((
                    path.clone(),
                    BridgeError::invalid_params(format!(
                        "vcs.stage: {path} is a submodule entry, so the status scan does not \
                         cover it; refusing to stage it"
                    )),
                ));
                failed_paths.insert(path.clone());
                continue;
            }
            targets.push(path.clone());
            continue;
        }
        let on_disk = match on_disk_spelling(path) {
            Ok(on_disk) => on_disk,
            Err(error) => {
                refusals.push((path.clone(), error));
                failed_paths.insert(path.clone());
                continue;
            }
        };
        if on_disk == *path {
            let variants = case_variant_names(index, path);
            if variants.len() > 1 {
                refusals.push((
                    path.clone(),
                    BridgeError::invalid_params(format!(
                        "vcs.stage: {path} is ambiguous with {} indexed entries that differ only in \
                         case; refusing to change another index entry",
                        variants.len()
                    )),
                ));
                failed_paths.insert(path.clone());
                continue;
            }
            targets.push(path.clone());
            continue;
        }
        if (0..4).any(|stage| index.tracked(&on_disk, stage)) {
            refusals.push((
                path.clone(),
                BridgeError::invalid_params(format!(
                    "vcs.stage: {path} names a case variant of the indexed entry {on_disk}; \
                     refusing to change another index entry"
                )),
            ));
            failed_paths.insert(path.clone());
            continue;
        }
        targets.push(on_disk);
    }
    targets
}

/// Unstage exactly one index entry.
///
/// The entry is matched by its literal repository-relative name, so a request
/// can never change an index entry it did not name. The HEAD version of that
/// entry is restored: the path itself, or — when the staged change renamed
/// it — the name the path had in HEAD. A staged deletion (the path is in HEAD
/// but no longer in the index) restores the HEAD entry; a path that is in
/// neither is not a staged change and is refused.
async fn unstage_exact_entry(
    index: &mut git_internal::internal::index::Index,
    path: &str,
    head_entries: &mut Option<
        std::collections::HashMap<
            String,
            (
                git_internal::internal::object::tree::TreeItemMode,
                git_internal::hash::ObjectHash,
            ),
        >,
    >,
    renames: &mut Option<Vec<RenameGroup>>,
    requested: &[String],
    case_insensitive: bool,
) -> Result<(), BridgeError> {
    use git_internal::internal::index::IndexEntry;

    use crate::utils::object_ext::BlobExt;

    let mut tracked = false;
    for stage in 0..4 {
        if index.tracked(path, stage) {
            tracked = true;
            break;
        }
    }

    if head_entries.is_none() {
        *head_entries = Some(head_tree_entries().await?);
    }
    let head_entries = head_entries
        .as_ref()
        .ok_or_else(|| BridgeError::internal("vcs.unstage: HEAD entries were not loaded"))?;
    if renames.is_none() {
        *renames = Some(proven_staged_renames(head_entries, index).await?);
    }
    let renames = renames
        .as_ref()
        .ok_or_else(|| BridgeError::internal("vcs.unstage: staged renames were not loaded"))?;
    // A staged rename has two or more endpoints. Undoing it restores the HEAD
    // path and drops every new path, so the request must authorize all of
    // them: naming only some of them would leave the rest of the rename
    // staged. The group that matters here is the one the CURRENT path belongs
    // to: a rename never changes another selected path's index entry.
    let mut rename_source: Option<String> = None;
    if let Some(group) = renames.iter().find(|group| {
        group.sources.iter().any(|source| source == path)
            || group.destinations.iter().any(|entry| entry == path)
    }) {
        // The authorization compares the repository entry names exactly: two
        // entries that differ only in case are two entries, and a request that
        // names one of them does not name the other.
        let named = |candidate: &str| requested.iter().any(|name| name == candidate);
        let paths = group
            .sources
            .iter()
            .chain(group.destinations.iter())
            .map(|entry| entry.as_str());
        // Two spellings that differ only in case name the same path only on a
        // case-folding filesystem; on a case-sensitive filesystem they are two
        // different paths and each one needs its own authorization.
        let single_spelling = case_insensitive
            && group.sources.len() == 1
            && group.destinations.len() == 1
            && group.destinations[0].eq_ignore_ascii_case(&group.sources[0]);
        if !single_spelling && !paths.clone().all(named) {
            return Err(BridgeError::invalid_params(format!(
                "vcs.unstage: {} is a staged rename to {}; name every path of the rename to undo it",
                group.sources.join(", "),
                group.destinations.join(", ")
            )));
        }
        // A source restores itself. A destination restores the group's single
        // source; with several sources the destination is ambiguous, so it is
        // only dropped and each source is restored by its own path.
        rename_source = if group.sources.iter().any(|source| source == path) {
            Some(path.to_string())
        } else if group.sources.len() == 1 {
            group.sources.first().cloned()
        } else {
            None
        };
    }
    // A staged rename is restored under its HEAD name, so the index keeps the
    // repository's own spelling.
    let head_name = rename_source.unwrap_or_else(|| path.to_string());

    // The HEAD entry is looked up by the requested name. A case-insensitive
    // lookup is only correct on a case-insensitive filesystem, where the two
    // spellings name the same file; on a case-sensitive filesystem they are
    // different entries and the fallback could change an entry the request did
    // not name.
    let mut head_entry = head_entries
        .get(&head_name)
        .copied()
        .map(|entry| (head_name.clone(), entry));
    if head_entry.is_none() && case_insensitive {
        let variant = head_entries
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&head_name))
            .map(|(name, entry)| (name.clone(), *entry));
        if let Some((variant_name, entry)) = variant {
            let case_entries: Vec<(u8, String)> = (0..4)
                .flat_map(|stage| {
                    index
                        .tracked_entries(stage)
                        .into_iter()
                        .map(move |entry| (stage, entry.name.clone()))
                })
                .filter(|(_, name)| name.eq_ignore_ascii_case(path))
                .collect();
            if case_entries.len() != 1 || case_entries[0].0 != 0 || case_entries[0].1 != path {
                return Err(BridgeError::invalid_params(format!(
                    "vcs.unstage: {path} is not the exact unambiguous index spelling for HEAD path \
                     {variant_name}; refusing to change another index entry"
                )));
            }
            head_entry = Some((variant_name, entry));
        }
    }

    if !tracked && head_entry.is_none() {
        return Err(BridgeError::invalid_params(format!(
            "vcs.unstage: {path} did not match any staged file"
        )));
    }

    // The staged entry is dropped first: a case-only rename cannot leave both
    // spellings in the index.
    for stage in 0..4 {
        index.remove(path, stage);
    }
    let Some((head_path, (head_mode, oid))) = head_entry else {
        // The path is not in HEAD: the staged addition is simply dropped.
        return Ok(());
    };
    let mode = index_mode_for_tree_item(head_mode)?;
    // A gitlink names a commit in another repository, which is normally absent
    // from this object store; its index entry carries no content, so no blob is
    // read for it.
    let size = if matches!(
        head_mode,
        git_internal::internal::object::tree::TreeItemMode::Commit
    ) {
        0
    } else {
        let blob = <git_internal::internal::object::blob::Blob as BlobExt>::load(&oid).map_err(
            |error| vcs::internal_hiding_host_paths("vcs.unstage: read HEAD blob", error),
        )?;
        u32::try_from(blob.data.len()).unwrap_or(u32::MAX)
    };
    let mut entry = IndexEntry::new_from_blob(head_path.to_string(), oid, size);
    entry.mode = mode;
    crate::utils::index_ext::update_preserving_flags(index, entry);
    Ok(())
}

/// The entries of HEAD's tree, keyed by their repository-relative path.
///
/// The value keeps the tree mode, so a gitlink (a submodule entry) is
/// recognized and never read as a blob.
///
/// An unborn HEAD has no tree: it is treated as an empty tree, so a staged
/// addition can be unstaged before the repository's first commit.
async fn head_tree_entries() -> Result<
    std::collections::HashMap<
        String,
        (
            git_internal::internal::object::tree::TreeItemMode,
            git_internal::hash::ObjectHash,
        ),
    >,
    BridgeError,
> {
    use crate::utils::object_ext::{CommitExt, TreeExt};

    let Some(head) = crate::internal::head::Head::current_commit_result()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs write: read HEAD", error))?
    else {
        return Ok(std::collections::HashMap::new());
    };
    let commit = <git_internal::internal::object::commit::Commit as CommitExt>::load(&head)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs write: load HEAD", error))?;
    let tree = <git_internal::internal::object::tree::Tree as TreeExt>::load(&commit.tree_id)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs write: load HEAD tree", error))?;
    let items = tree
        .get_plain_items_with_mode()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs write: read HEAD tree", error))?;
    Ok(items
        .into_iter()
        // The entry name is kept verbatim: a literal backslash is a valid
        // character in a repository path, and rewriting it would let two
        // distinct entries share one key.
        .map(|(path, oid, mode)| (path.display().to_string(), (mode, oid)))
        .collect())
}

/// The index mode a HEAD tree entry is stored under.
fn index_mode_for_tree_item(
    mode: git_internal::internal::object::tree::TreeItemMode,
) -> Result<u32, BridgeError> {
    use git_internal::internal::object::tree::TreeItemMode;
    match mode {
        TreeItemMode::Blob => Ok(0o100644),
        TreeItemMode::BlobExecutable => Ok(0o100755),
        TreeItemMode::Link => Ok(0o120000),
        TreeItemMode::Commit => Ok(0o160000),
        // INVARIANT: a tree entry is expanded into its files before it
        // reaches the index, so a tree never names an index entry.
        TreeItemMode::Tree => Err(BridgeError::internal(
            "vcs.unstage: HEAD holds a tree entry, which is not an index entry",
        )),
    }
}

/// A proven staged rename: the HEAD paths and the index paths that carry the
/// same content.
///
/// The group holds every path that could take part in the change, so an
/// ambiguous case (several identical sources or several identical
/// destinations) is authorized as a whole instead of guessed.
struct RenameGroup {
    sources: Vec<String>,
    destinations: Vec<String>,
}

/// The staged renames the index proves.
///
/// A rename is proven when a path that HEAD has and the index no longer tracks
/// is paired with the paths the index added that carry the same content. A
/// rename that also changed the content is not proven here: the unstage then
/// behaves like an ordinary restore, exactly as the repository's own restore
/// does, and the commit scope check still refuses a commit that names only
/// one side of the change.
async fn proven_staged_renames(
    head_entries: &std::collections::HashMap<
        String,
        (
            git_internal::internal::object::tree::TreeItemMode,
            git_internal::hash::ObjectHash,
        ),
    >,
    index: &git_internal::internal::index::Index,
) -> Result<Vec<RenameGroup>, BridgeError> {
    let mut deleted_by_oid: std::collections::HashMap<git_internal::hash::ObjectHash, Vec<String>> =
        std::collections::HashMap::new();
    for (path, (_mode, oid)) in head_entries {
        let mut tracked = false;
        for stage in 0..4 {
            if index.tracked(path, stage) {
                tracked = true;
                break;
            }
        }
        if tracked {
            continue;
        }
        deleted_by_oid.entry(*oid).or_default().push(path.clone());
    }
    let mut groups: std::collections::HashMap<
        git_internal::hash::ObjectHash,
        (Vec<String>, Vec<String>),
    > = std::collections::HashMap::new();
    for entry in index.tracked_entries(0) {
        let path = entry.name.clone();
        if head_entries.contains_key(&path) {
            continue;
        }
        // The same content under a new name is a rename. Every path that
        // carries that content joins the same group, so an ambiguous case is
        // authorized as a whole rather than resolved by a guess.
        groups.entry(entry.hash).or_default().1.push(path);
    }
    let mut renames: Vec<RenameGroup> = groups
        .into_iter()
        .filter_map(|(oid, (mut sources, mut destinations))| {
            let deleted = deleted_by_oid.remove(&oid)?;
            sources.extend(deleted);
            if sources.is_empty() || destinations.is_empty() {
                return None;
            }
            // The members are sorted, so the refusal message and the pairing
            // do not depend on the iteration order.
            sources.sort();
            destinations.sort();
            Some(RenameGroup {
                sources,
                destinations,
            })
        })
        .collect();
    // A stable order keeps the refusal message and the pairing deterministic.
    renames.sort_by(|left, right| {
        left.sources
            .first()
            .cmp(&right.sources.first())
            .then_with(|| left.destinations.cmp(&right.destinations))
    });
    Ok(renames)
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
    refusals: &mut Vec<(String, BridgeError)>,
    failed_paths: &mut BTreeSet<String>,
) {
    let Ok(workdir) = util::try_working_dir() else {
        refusals.push((
            String::new(),
            BridgeError::scope_mismatch("not_a_repository: vcs.stage needs a worktree"),
        ));
        return;
    };
    for plan in plans.iter_mut() {
        let StagePlan::File {
            path, bytes, clean, ..
        } = plan
        else {
            continue;
        };
        if failed_paths.contains(path) {
            continue;
        }
        let selected = crate::command::status_io_worker::selected_index_oid(index, Path::new(path));
        let converted = crate::utils::worktree_conversion::clean_captured_bytes(
            &workdir,
            Path::new(path),
            bytes.clone(),
            || crate::command::status_io_worker::index_has_text_crlf(selected),
        )
        .map_err(map_stage_conversion_error);
        match converted {
            Ok(converted) => *clean = converted,
            Err(error) => {
                refusals.push((path.clone(), error));
                failed_paths.insert(path.clone());
            }
        }
    }
}

fn confirm_stage_plan(plan: &StagePlan) -> Result<(), BridgeError> {
    match plan {
        StagePlan::File {
            path, bytes, mode, ..
        } => match read_optional_worktree_bytes(path)? {
            Some((now, now_mode)) if now == *bytes && now_mode == *mode => Ok(()),
            Some(_) => Err(BridgeError::stale_fence(format!(
                "stale_state: {path} changed after its snapshot; no index write was performed"
            ))),
            None => Err(BridgeError::stale_fence(format!(
                "stale_state: {path} disappeared after its snapshot; no index write was performed"
            ))),
        },
        StagePlan::Link { path, target } => {
            if symlink_target(path)?.as_deref() == Some(target.as_slice()) {
                Ok(())
            } else {
                Err(BridgeError::stale_fence(format!(
                    "stale_state: symlink {path} changed after its snapshot; no index write was performed"
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

/// Recheck a stage source at the final index publication boundary.
fn confirm_prepared_stage_plan(plan: &StagePlan) -> Result<(), BridgeError> {
    confirm_stage_plan(plan).map_err(|_| {
        BridgeError::stale_fence(format!(
            "stale_state: {} changed while the batch was prepared; no index write was performed",
            plan.path()
        ))
    })
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
    plan: &StagePlan,
    index: &mut git_internal::internal::index::Index,
) -> Result<(), BridgeError> {
    use git_internal::internal::object::types::ObjectType;
    match plan {
        // The stored representation was resolved for every selected path
        // before publication began, and the case-alias check ran in the same
        // preparation pass, so this step does not refuse for those reasons.
        StagePlan::File {
            path, clean, mode, ..
        } => {
            let snapshot =
                git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, clean);
            publish_snapshot(path, index, clean, snapshot, *mode).await
        }
        StagePlan::Link { path, target } => {
            let snapshot =
                git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, target);
            publish_snapshot(path, index, target, snapshot, 0o120000).await
        }
        StagePlan::Delete { path } => stage_tracked_deletion(path, index).await,
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
    remove_case_aliases(index, path).await?;
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

/// The case alias an index entry may replace, if any.
///
/// The check is separate from the removal so a batch can discover every
/// refusal before anything is published.
async fn case_alias_to_remove(
    index: &git_internal::internal::index::Index,
    path: &str,
) -> Result<Option<String>, BridgeError> {
    let case_entries: Vec<(u8, String)> = (0..4)
        .flat_map(|stage| {
            index
                .tracked_entries(stage)
                .into_iter()
                .map(move |entry| (stage, entry.name.clone()))
        })
        .filter(|(_, name)| name.eq_ignore_ascii_case(path))
        .collect();
    if case_entries.iter().all(|(_, name)| name == path) {
        return Ok(None);
    }

    let refused = || {
        BridgeError::invalid_params(format!(
            "vcs.stage: ambiguous case variant of {path}; refusing to change another index entry"
        ))
    };
    let Ok(workdir) = util::try_working_dir() else {
        return Err(refused());
    };
    if !crate::utils::path_case::probe_dir_ignore_case(&workdir) {
        return Err(refused());
    }
    let root = crate::utils::beneath::open_root(&workdir).map_err(|_| refused())?;
    let [(0, source)] = case_entries.as_slice() else {
        return Err(refused());
    };
    if source == path
        || crate::utils::beneath::same_nofollow_entry(&root, Path::new(path), Path::new(source))
            .ok()
            != Some(true)
    {
        return Err(refused());
    }

    // Removing a case alias is allowed only for an unmodified HEAD spelling
    // that is the sole index entry at this case-folded name. This recognizes a
    // case-only rename while preserving any separately staged index spelling.
    let head_entries = head_tree_entries().await?;
    let head_aliases: Vec<_> = head_entries
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(path))
        .collect();
    if head_aliases.len() != 1 || head_aliases[0].0 != source {
        return Err(refused());
    }
    let (head_mode, head_oid) = head_aliases[0].1;
    let Some(index_entry) = index.get(source, 0) else {
        return Err(refused());
    };
    if index_entry.hash != *head_oid || index_entry.mode != index_mode_for_tree_item(*head_mode)? {
        return Err(refused());
    }

    Ok(Some(source.clone()))
}

/// Remove the case alias an index entry may replace.
///
/// The check that decides whether the alias may be removed lives in
/// [`case_alias_to_remove`], so a batch can run it for every path before it
/// publishes anything.
async fn remove_case_aliases(
    index: &mut git_internal::internal::index::Index,
    path: &str,
) -> Result<(), BridgeError> {
    if let Some(source) = case_alias_to_remove(index, path).await? {
        index.remove(&source, 0);
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

async fn stage_tracked_deletion(
    path: &str,
    index: &mut git_internal::internal::index::Index,
) -> Result<(), BridgeError> {
    remove_case_aliases(index, path).await?;
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
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: read index", error))?;
    let unstaged = changes_to_be_staged()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: read worktree", error))?;
    worktree_generation_from(workdir, &staged, &unstaged).await
}

/// The worktree generation of a given observation of the changed paths.
async fn worktree_generation_from(
    workdir: &Path,
    staged: &crate::command::status::Changes,
    unstaged: &crate::command::status::Changes,
) -> Result<String, BridgeError> {
    let root = crate::utils::beneath::open_root(workdir)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs fence: cannot pin root", error))?;
    let identity =
        crate::utils::beneath::entry_identity_beneath(&root, Path::new("")).map_err(|error| {
            vcs::stale_fence_hiding_host_paths("vcs fence: cannot identify workdir root", error)
        })?;
    confirm_workdir_root(workdir, &identity)?;
    let total = change_count(staged).saturating_add(change_count(unstaged));
    if total > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    let mut rows = BTreeSet::new();
    let mut remaining = MAX_FENCE_BYTES;
    let mut digests = BTreeMap::new();
    collect_rows(
        &root,
        staged,
        "staged",
        &mut rows,
        &mut remaining,
        &mut digests,
    )?;
    collect_rows(
        &root,
        unstaged,
        "unstaged",
        &mut rows,
        &mut remaining,
        &mut digests,
    )?;
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

fn verify_all_observed_stamps(
    root: &std::fs::File,
    stamps: &crate::command::status::WorktreeStamps,
) -> Result<(), BridgeError> {
    for (path, expected) in stamps {
        let actual = crate::command::status::capture_worktree_entry(root, path).map_err(|_| {
            BridgeError::stale_fence(format!(
                "vcs fence: {} changed after the status scan",
                path.display()
            ))
        })?;
        let matches = match (expected, actual.as_ref()) {
            (crate::command::status::ObservedPath::Absent, None) => true,
            (crate::command::status::ObservedPath::Observed(expected), Some((identity, stat))) => {
                expected.identity == *identity && expected.stat == *stat
            }
            _ => false,
        };
        if !matches {
            return Err(BridgeError::stale_fence(format!(
                "vcs fence: {} changed after the status scan",
                path.display()
            )));
        }
    }
    Ok(())
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
    digests: &mut BTreeMap<std::path::PathBuf, String>,
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
        // The repository-relative path is used verbatim: rewriting it would
        // hash another file than the one that changed.
        let rel = path.display().to_string();
        // A path that is both staged and unstaged is read once: the fence
        // covers its content once, and the repeated row reuses that digest
        // instead of reading the file again.
        let digest = match digests.get(path) {
            Some(digest) => digest.clone(),
            None => {
                let digest = content_digest(root, &rel, remaining)?;
                digests.insert(path.clone(), digest.clone());
                digest
            }
        };
        rows.insert(format!("{state}\t{rel}\t{digest}"));
    }
    for (old, new) in &changes.renamed {
        let rel = new.display().to_string();
        let from = old.display().to_string();
        let digest = match digests.get(new) {
            Some(digest) => digest.clone(),
            None => {
                let digest = content_digest(root, &rel, remaining)?;
                digests.insert(new.clone(), digest.clone());
                digest
            }
        };
        rows.insert(format!("rename\t{from}\t{rel}\t{digest}"));
    }
    Ok(())
}

/// Charge bytes a fence read consumed against the aggregate budget.
fn charge_fence_bytes(remaining: &mut u64, read: u64) -> Result<(), BridgeError> {
    // The bytes are consumed before the refusal is reported, so a caller that
    // continues cannot reuse an allowance those bytes already spent.
    let over_budget = read > *remaining;
    *remaining = remaining.saturating_sub(read);
    if over_budget {
        return Err(BridgeError::invalid_params(
            "vcs fence aggregate byte budget exceeded",
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
        // Every read is charged as it happens, including a read that is
        // followed by a refusal.
        let (target, first_read) =
            crate::utils::beneath::read_symlink_beneath_counted(root, relative_path);
        charge_fence_bytes(remaining, first_read)?;
        let target = target.map_err(|error| {
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
        let (target_after, second_read) =
            crate::utils::beneath::read_symlink_beneath_counted(root, relative_path);
        charge_fence_bytes(remaining, second_read)?;
        let target_after = target_after.map_err(|error| {
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
    let read_result = (&file).take(available + 1).read_to_end(&mut bytes);
    // The bytes that were read are charged on every outcome, also when the
    // read failed or exceeded the allowance.
    charge_fence_bytes(remaining, bytes.len() as u64)?;
    read_result.map_err(|error| {
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

    /// The fence generation follows the content the status scan captured: a
    /// later rewrite of the same length produces different fence rows, so the
    /// write refuses the stale fence even when the path's metadata was
    /// preserved.
    #[test]
    fn a_rewritten_path_invalidates_the_scanned_fence() {
        use std::path::PathBuf;

        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("file"), b"one").expect("write");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let mut budget = 1024u64;
        let stamp =
            crate::command::status::capture_worktree_stamp(&root, Path::new("file"), &mut budget)
                .expect("stamp")
                .expect("present");
        let mut stamps = crate::command::status::WorktreeStamps::new();
        stamps.insert(
            PathBuf::from("file"),
            crate::command::status::ObservedPath::Observed(stamp),
        );
        let changes = crate::command::status::Changes {
            modified: vec![PathBuf::from("file")],
            ..Default::default()
        };

        let mut scanned_rows = BTreeSet::new();
        let mut scanned_budget = MAX_FENCE_BYTES;
        let mut charged = BTreeSet::new();
        collect_rows_from_stamps(
            &changes,
            "unstaged",
            &mut scanned_rows,
            &stamps,
            &mut scanned_budget,
            &mut charged,
        )
        .expect("scanned rows");
        std::fs::write(dir.path().join("file"), b"two").expect("rewrite");
        let mut current_rows = BTreeSet::new();
        let mut current_budget = 1024u64;
        let mut digests = BTreeMap::new();
        collect_rows(
            &root,
            &changes,
            "unstaged",
            &mut current_rows,
            &mut current_budget,
            &mut digests,
        )
        .expect("current rows");

        assert_ne!(
            scanned_rows, current_rows,
            "a rewritten path must produce a different fence"
        );
    }

    #[test]
    fn commit_scope_keeps_case_colliding_index_names_distinct() {
        let changes = crate::command::status::Changes {
            new: vec!["Case.txt".into(), "case.txt".into()],
            ..Default::default()
        };
        let staged = staged_path_names(&changes);
        let requested = BTreeSet::from(["Case.txt".to_string()]);
        let error = require_exact_commit_scope(&staged, &requested)
            .expect_err("both index names require authorization");
        assert!(error.message.contains("do not match the staged set"));
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

    #[tokio::test]
    async fn case_alias_does_not_delete_a_distinct_index_entry() {
        use git_internal::internal::{
            index::{Index, IndexEntry},
            object::types::ObjectType,
        };
        let mut index = Index::new();
        let oid = git_internal::hash::ObjectHash::from_type_and_data(ObjectType::Blob, b"first");
        index.add(IndexEntry::new_from_blob("Name".into(), oid, 5));
        assert!(remove_case_aliases(&mut index, "name").await.is_err());
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
        let mut digests = BTreeMap::new();
        let result = collect_rows(
            &root,
            &changes,
            "unstaged",
            &mut rows,
            &mut 32_000_000,
            &mut digests,
        );
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
    fn a_symlink_target_is_charged_for_both_reads() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("referent"), b"0123456789").expect("referent");
        // `readlink` returns the six-byte target text, not the referent.
        symlink("target", dir.path().join("link")).expect("symlink");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");

        // Six bytes cover one read of the link target but not the two reads the
        // recheck performs, so this distinguishes double charging from single
        // charging.
        let mut remaining = 6u64;
        let error = content_digest(&root, "link", &mut remaining)
            .expect_err("both symlink reads are charged");
        assert!(
            error.message.contains("budget"),
            "the refusal names the budget: {}",
            error.message
        );
        assert_eq!(remaining, 0, "the first read is still charged");
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
    #[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
    fn a_failed_symlink_retry_charges_the_first_read() {
        let dir = tempfile::tempdir().expect("fixture");
        let target = "a".repeat(300);
        symlink(&target, dir.path().join("link")).expect("symlink");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");

        crate::utils::beneath::set_fail_readlink_retry_for_test(true);
        let mut remaining = 32_000_000u64;
        let error =
            content_digest(&root, "link", &mut remaining).expect_err("the retried read fails");
        assert!(
            error.message.contains("symlink") || error.message.contains("link"),
            "{}",
            error.message
        );
        assert_eq!(
            remaining,
            32_000_000 - 256,
            "the first attempt's bytes are charged before the retry fails"
        );
    }

    #[test]
    fn a_long_symlink_target_is_charged_for_its_retried_reads() {
        let dir = tempfile::tempdir().expect("fixture");
        // A target longer than the reader's first 256-byte buffer is read
        // twice inside the helper: once filling the buffer and once for the
        // whole target.
        let target = "a".repeat(300);
        symlink(&target, dir.path().join("link")).expect("symlink");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");

        // One attempt reads 256 bytes and the retried attempt reads the whole
        // 300-byte target, so the recheck consumes 556 bytes twice: an 800-byte
        // allowance refuses it, and charging only the target length twice
        // (600 bytes) would have accepted it.
        let mut remaining = 800u64;
        let error = content_digest(&root, "link", &mut remaining)
            .expect_err("the retried reads are charged");
        assert!(
            error.message.contains("budget"),
            "the refusal names the budget: {}",
            error.message
        );
        assert!(
            remaining < 800,
            "the first read is charged before the retried read fails"
        );
    }

    #[test]
    fn aggregate_budget_refuses_file_larger_than_remaining_budget() {
        let dir = tempfile::tempdir().expect("fixture");
        std::fs::write(dir.path().join("file"), b"12345").expect("write");
        let root = crate::utils::beneath::open_root(dir.path()).expect("root");
        let error = content_digest(&root, "file", &mut 4).expect_err("budget");
        assert!(
            error.message.contains("budget") || error.message.contains("oversized"),
            "{}",
            error.message
        );
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
