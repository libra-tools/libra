//! Client-side object storage gateway.
//!
//! This module is the synchronous facade that the rest of the codebase uses to read,
//! write, and search Git objects. It hides three orthogonal concerns:
//!
//! 1. **Storage backend selection** — local-only, or local cache plus a remote
//!    object_store-backed bucket (S3/R2). Backend is chosen at construction time from
//!    `LIBRA_STORAGE_*` environment variables and `vault.env.*` config entries.
//! 2. **Sync/async bridging** — most of the codebase is synchronous CLI logic, while
//!    every storage backend is async. A dedicated multi-thread Tokio runtime owned by
//!    this module runs the async work and the CLI thread blocks on a `mpsc::channel`,
//!    avoiding nested-runtime panics that would occur if we drove the storage from the
//!    main runtime.
//! 3. **Background object indexing** — every successful `put` enqueues an index-update
//!    message for the cloud-backup object index. The consumer runs serially on the
//!    background runtime so concurrent writers cannot deadlock on the SQLite database.
//!
//! Search supports Git's revision navigation suffixes (`HEAD`, `~`, `^`).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use futures::FutureExt; // Import for catch_unwind
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{commit::Commit, types::ObjectType},
};
use once_cell::sync::Lazy;
use regex::Regex;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Statement,
    Value,
};
use serde::{Deserialize, Serialize};
use tokio::{
    runtime::Runtime,
    sync::mpsc::{Receiver, Sender, channel, error::TrySendError},
};
use uuid::Uuid;

use crate::{
    command::load_object,
    internal::{
        branch::Branch,
        config::{ConfigKv, decrypt_value},
        db,
        db::establish_connection_with_busy_timeout,
        head::Head,
        model::object_index,
    },
    utils::{
        storage::{Storage, local::LocalStorage, remote::RemoteStorage, tiered::TieredStorage},
        util::{DATABASE, try_get_storage_path},
    },
};

// Dedicated runtime for storage operations to avoid blocking/deadlocks in the main runtime.
// We never `await` storage from the calling tokio runtime; instead we hand the work to
// this private runtime and block on an mpsc receiver. This avoids `block_on within
// runtime` panics and decouples storage IO from the caller's executor.
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    // INVARIANT: `Builder::build()` only fails on platform resource
    // exhaustion (cannot spawn the I/O reactor or worker threads). If
    // that happens the process cannot make progress regardless, so
    // surfacing the panic immediately is the right behavior. The
    // panic message identifies that this is the storage runtime so
    // the failure is distinguishable from caller-runtime issues.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build dedicated tokio runtime for ClientStorage IO")
});

// Message describing a single object_index update queued by `ClientStorage::put`.
// Carries enough state for the consumer to run independently of the calling thread.
struct IndexUpdateMsg {
    hash: String,
    obj_type: String,
    size: i64,
    db_path: PathBuf,
    marker_path: Option<PathBuf>,
    // Replay keeps this cross-process lock through the SQLite upsert and
    // marker retirement. Queued writers acquire the same bounded OID-shard lock
    // immediately before touching SQLite, so a marker retired by replay is a
    // durable ownership fence rather than a racy existence hint.
    _marker_lock: Option<Arc<ObjectIndexRepairLock>>,
    failure_counter: Arc<AtomicUsize>,
    pending_counter: Arc<AtomicUsize>,
}

const INDEX_REPAIR_MARKER_DIR: &str = "object-index-repair";
const INDEX_REPAIR_MARKER_STAGING_DIR: &str = "object-index-repair-tmp";
const INDEX_REPAIR_LOCK_DIR: &str = "object-index-repair-locks";
const INDEX_REPAIR_GENERATION_LOCK: &str = "object-index-repair-generation.lock";
// Four hexadecimal digits cap the persistent cross-process lock namespace at
// 65,536 files while keeping unrelated object writes well distributed.
const INDEX_REPAIR_LOCK_SHARD_HEX_LEN: usize = 4;
const INDEX_REPAIR_MARKER_SCHEMA_VERSION: u8 = 1;
const INDEX_REPAIR_MARKER_READ_CAP: u64 = 16 * 1024;
const INDEX_REPAIR_STAGING_SCAN_CAP: usize = 1_024;
const INDEX_REPAIR_STAGING_REMOVE_CAP: usize = 256;
const INDEX_REPAIR_STAGING_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(not(test))]
const INDEX_REPAIR_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const INDEX_REPAIR_LOCK_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
const INDEX_REPAIR_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const INDEX_REPAIR_MARKER_PAGE_CAP: usize = 100_000;
#[cfg(test)]
const INDEX_REPAIR_MARKER_PAGE_CAP: usize = 3;
#[cfg(not(test))]
const INDEX_REPAIR_BATCH_SIZE: usize = 100;
#[cfg(test)]
const INDEX_REPAIR_BATCH_SIZE: usize = 2;

struct PendingObjectIndexPage {
    updates: Vec<IndexUpdateMsg>,
    has_more: bool,
    // Held from the directory snapshot through the batch upsert and durable
    // marker retirement. Publishers, queued writers, replay, and destructive
    // deletion therefore observe one total order for marker generations.
    _generation_lock: Option<ObjectIndexRepairLock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectIndexRepairOutcome {
    pub(crate) repaired: usize,
    pub(crate) remaining: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingObjectIndexUpdate {
    schema_version: u8,
    o_id: String,
    o_type: String,
    o_size: i64,
}

/// Process-crash-safe advisory ownership for one bounded OID shard. Lock files
/// remain stable after marker retirement so a delayed writer cannot acquire a
/// newly created inode and bypass a replay process that still owns the old one.
#[derive(Debug)]
struct ObjectIndexRepairLock {
    #[cfg_attr(not(unix), allow(dead_code))]
    file: fs::File,
}

fn index_repair_lock_shard(oid: &str) -> io::Result<String> {
    if !valid_supported_index_repair_oid(oid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "object-index repair lock identity contains an invalid object id",
        ));
    }
    Ok(oid[..INDEX_REPAIR_LOCK_SHARD_HEX_LEN].to_string())
}

fn index_repair_lock_path(db_path: &Path, oid: &str) -> io::Result<PathBuf> {
    let lock_shard = index_repair_lock_shard(oid)?;
    let storage_dir = db_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "repository database path has no storage directory: {}",
                db_path.display()
            ),
        )
    })?;
    Ok(storage_dir
        .join(INDEX_REPAIR_LOCK_DIR)
        .join(format!("{lock_shard}.lock")))
}

fn index_repair_generation_lock_path(db_path: &Path) -> io::Result<PathBuf> {
    let storage_dir = db_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "repository database path has no storage directory: {}",
                db_path.display()
            ),
        )
    })?;
    Ok(storage_dir
        .join(INDEX_REPAIR_LOCK_DIR)
        .join(INDEX_REPAIR_GENERATION_LOCK))
}

#[cfg(unix)]
fn open_index_repair_lock_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(windows)]
fn open_index_repair_lock_file(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        // A zero share mode is released by the kernel on process death and is
        // Windows' equivalent of the Unix advisory lock used below.
        .share_mode(0)
        .open(path)
}

#[cfg(all(not(unix), not(windows)))]
fn open_index_repair_lock_file(_path: &Path) -> io::Result<fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cross-process object-index repair locking is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn try_acquire_index_repair_lock_file(path: &Path) -> io::Result<Option<ObjectIndexRepairLock>> {
    use std::os::fd::AsRawFd;

    let file = open_index_repair_lock_file(path)?;
    // SAFETY: flock operates on an owned descriptor and does not outlive it.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(Some(ObjectIndexRepairLock { file }));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(error),
    }
}

#[cfg(windows)]
fn try_acquire_index_repair_lock_file(path: &Path) -> io::Result<Option<ObjectIndexRepairLock>> {
    match open_index_repair_lock_file(path) {
        Ok(file) => Ok(Some(ObjectIndexRepairLock { file })),
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION mean another
        // process owns this zero-share handle.
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn try_acquire_index_repair_lock_file(path: &Path) -> io::Result<Option<ObjectIndexRepairLock>> {
    open_index_repair_lock_file(path).map(|file| Some(ObjectIndexRepairLock { file }))
}

fn try_acquire_index_repair_lock(
    db_path: &Path,
    oid: &str,
) -> io::Result<Option<ObjectIndexRepairLock>> {
    try_acquire_index_repair_lock_file(&index_repair_lock_path(db_path, oid)?)
}

fn acquire_index_repair_lock_file(
    lock_path: &Path,
    identity: &str,
) -> io::Result<ObjectIndexRepairLock> {
    let started = Instant::now();
    loop {
        if let Some(lock) = try_acquire_index_repair_lock_file(lock_path)? {
            return Ok(lock);
        }
        if started.elapsed() >= INDEX_REPAIR_LOCK_WAIT_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for object-index repair lock '{}' for {identity}; another Libra process may be stalled",
                    lock_path.display()
                ),
            ));
        }
        std::thread::sleep(INDEX_REPAIR_LOCK_RETRY_INTERVAL);
    }
}

fn acquire_index_repair_lock(db_path: &Path, oid: &str) -> io::Result<ObjectIndexRepairLock> {
    acquire_index_repair_lock_file(
        &index_repair_lock_path(db_path, oid)?,
        &format!("object {oid}"),
    )
}

fn acquire_index_repair_generation_lock(db_path: &Path) -> io::Result<ObjectIndexRepairLock> {
    acquire_index_repair_lock_file(
        &index_repair_generation_lock_path(db_path)?,
        "repair-marker generation",
    )
}

#[cfg(unix)]
impl Drop for ObjectIndexRepairLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        // SAFETY: the descriptor is owned by this guard. Closing would also
        // release the lock; explicit unlock makes the lifetime obvious.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Repository-wide fence for destructive `object_index` deletion. Marker
/// publishers take the same generation lock before their atomic rename. Once
/// this guard has verified that none of the candidate OIDs has a durable
/// marker, holding it through the SQLite commit prevents a delayed publisher
/// from recreating one of the deleted rows afterward.
#[derive(Debug)]
pub(crate) struct ObjectIndexDeletionFence {
    _generation_lock: ObjectIndexRepairLock,
}

pub(crate) async fn acquire_object_index_deletion_fence(
    db_path: &Path,
    oids: &[String],
) -> io::Result<Option<ObjectIndexDeletionFence>> {
    if oids.is_empty() {
        return Ok(None);
    }
    let db_path = db_path.to_path_buf();
    let oids = oids.iter().cloned().collect::<HashSet<_>>();
    tokio::task::spawn_blocking(move || {
        if let Some(invalid) = oids
            .iter()
            .find(|oid| !valid_supported_index_repair_oid(oid))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot fence object-index deletion for invalid object id {invalid}"),
            ));
        }

        let generation_lock = acquire_index_repair_generation_lock(&db_path)?;
        let marker_dir = db_path
            .parent()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "repository database path has no storage directory: {}",
                        db_path.display()
                    ),
                )
            })?
            .join(INDEX_REPAIR_MARKER_DIR);
        let entries = match fs::read_dir(&marker_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Some(ObjectIndexDeletionFence {
                    _generation_lock: generation_lock,
                }));
            }
            Err(error) => return Err(error),
        };

        for (scanned_entries, entry) in entries.enumerate() {
            if scanned_entries >= INDEX_REPAIR_MARKER_PAGE_CAP {
                return Err(io::Error::other(format!(
                    "refusing object-index deletion because repair-marker validation exceeded the bounded limit of {INDEX_REPAIR_MARKER_PAGE_CAP} entries"
                )));
            }
            let entry = entry?;
            let path = entry.path();
            let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
                io::Error::other(format!(
                    "object-index repair directory contains a non-UTF-8 entry: {}",
                    path.display()
                ))
            })?;
            let marker_oid = name
                .strip_suffix(".json")
                .and_then(|identity| identity.split_once('.'))
                .map(|(oid, _)| oid);
            if let Some(marker_oid) = marker_oid
                && oids.contains(marker_oid)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "refusing object-index deletion because object {} still has durable repair marker '{}'",
                        marker_oid,
                        path.display()
                    ),
                ));
            }
        }

        Ok(Some(ObjectIndexDeletionFence {
            _generation_lock: generation_lock,
        }))
    })
    .await
    .map_err(|error| io::Error::other(format!("object-index deletion fence task failed: {error}")))?
}

fn index_repair_marker_path(db_path: &Path, oid: &str, object_type: &str) -> io::Result<PathBuf> {
    if !valid_supported_index_repair_oid(oid) || !valid_index_repair_type(object_type) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "object-index repair identity contains an invalid object id or type",
        ));
    }
    let storage_dir = db_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "repository database path has no storage directory: {}",
                db_path.display()
            ),
        )
    })?;
    Ok(storage_dir
        .join(INDEX_REPAIR_MARKER_DIR)
        .join(format!("{oid}.{object_type}.json")))
}

fn persist_index_repair_marker(msg: &IndexUpdateMsg) -> io::Result<PathBuf> {
    let marker_path = index_repair_marker_path(&msg.db_path, &msg.hash, &msg.obj_type)?;
    // Marker creation participates in a repository-wide generation fence.
    // Destructive cleanup holds this lock from its final marker revalidation
    // through the catalog transaction, so a new durable repair job cannot be
    // published in the deletion window.
    let _generation_lock = acquire_index_repair_generation_lock(&msg.db_path)?;
    let _lock = acquire_index_repair_lock(&msg.db_path, &msg.hash)?;
    let marker = PendingObjectIndexUpdate {
        schema_version: INDEX_REPAIR_MARKER_SCHEMA_VERSION,
        o_id: msg.hash.clone(),
        o_type: msg.obj_type.clone(),
        o_size: msg.size,
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|error| io::Error::other(format!("encode object-index repair marker: {error}")))?;
    let staging_dir = msg
        .db_path
        .parent()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "repository database path has no storage directory: {}",
                    msg.db_path.display()
                ),
            )
        })?
        .join(INDEX_REPAIR_MARKER_STAGING_DIR);
    let mut writer = crate::utils::atomic_stream::StreamingAtomicFile::new_in(
        &staging_dir,
        crate::utils::atomic_write::sync_data_enabled(),
    )?;
    writer.write_all(&bytes)?;
    writer.persist(&marker_path)?;
    Ok(marker_path)
}

fn retire_index_repair_marker(path: &Path) -> io::Result<()> {
    if cfg!(debug_assertions)
        && std::env::var_os("LIBRA_TEST_OBJECT_INDEX_MARKER_RETIRE_FAIL").is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected object-index repair marker retirement failure",
        ));
    }
    match fs::remove_file(path) {
        Ok(()) => {
            if crate::utils::atomic_write::sync_data_enabled() {
                let parent = path.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "object-index repair marker has no parent directory: {}",
                            path.display()
                        ),
                    )
                })?;
                crate::utils::atomic_write::fsync_parent_dir(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn current_index_work_scope() -> IndexWorkScope {
    ACTIVE_INDEX_WORK_SCOPE
        .try_with(Clone::clone)
        .unwrap_or_else(|_| UNSCOPED_INDEX_WORK_SCOPE.clone())
}

fn current_index_failure_counter() -> Arc<AtomicUsize> {
    current_index_work_scope().failures
}

fn current_index_pending_counter() -> Arc<AtomicUsize> {
    current_index_work_scope().pending
}

fn record_index_update_failure(msg: &IndexUpdateMsg) {
    msg.failure_counter.fetch_add(1, Ordering::SeqCst);
}

fn enqueue_index_update(msg: IndexUpdateMsg, context: &'static str) {
    PENDING_TASKS.fetch_add(1, Ordering::Relaxed);
    msg.pending_counter.fetch_add(1, Ordering::Relaxed);
    let sender = if Arc::ptr_eq(&msg.pending_counter, &UNSCOPED_INDEX_WORK_SCOPE.pending) {
        &INDEX_UPDATE_CHANNELS.unscoped
    } else {
        &INDEX_UPDATE_CHANNELS.scoped
    };
    match sender.try_send(msg) {
        Ok(()) => {}
        Err(TrySendError::Full(msg)) => {
            let sender = sender.clone();
            RUNTIME.spawn(async move {
                if let Err(error) = sender.send(msg).await {
                    record_index_update_failure(&error.0);
                    error.0.pending_counter.fetch_sub(1, Ordering::Release);
                    PENDING_TASKS.fetch_sub(1, Ordering::Release);
                    tracing::warn!("Failed to queue {context}: channel closed");
                }
            });
        }
        Err(TrySendError::Closed(msg)) => {
            record_index_update_failure(&msg);
            msg.pending_counter.fetch_sub(1, Ordering::Release);
            PENDING_TASKS.fetch_sub(1, Ordering::Release);
            tracing::warn!("Failed to queue {context}: channel closed");
        }
    }
}

// RAII guard that decrements PENDING_TASKS exactly once even if the consumer task panics.
// Drop runs on both the success path and during unwinding, so the pending counter
// observed by `wait_for_background_tasks` cannot drift on errors.
struct TaskGuard {
    pending_counter: Arc<AtomicUsize>,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        // Release pairs with barrier Acquire loads so a foreground caller
        // that observes zero also observes the terminal failure counter write.
        self.pending_counter.fetch_sub(1, Ordering::Release);
        PENDING_TASKS.fetch_sub(1, Ordering::Release);
    }
}

fn register_pending_index_work(scope: &IndexWorkScope) -> TaskGuard {
    PENDING_TASKS.fetch_add(1, Ordering::Relaxed);
    scope.pending.fetch_add(1, Ordering::Relaxed);
    TaskGuard {
        pending_counter: Arc::clone(&scope.pending),
    }
}

// Invocation-scoped updates and unrelated direct-library updates use separate
// bounded FIFO lanes. A slow direct backlog therefore cannot consume a CLI
// invocation's finite drain budget. Each lane remains serial, while SQLite's
// bounded busy retry coordinates the rare case where both lanes target the
// same repository concurrently.
struct IndexUpdateChannels {
    scoped: Sender<IndexUpdateMsg>,
    unscoped: Sender<IndexUpdateMsg>,
}

static INDEX_UPDATE_CHANNELS: Lazy<IndexUpdateChannels> = Lazy::new(|| {
    let (scoped, scoped_rx) = channel::<IndexUpdateMsg>(1000);
    let (unscoped, unscoped_rx) = channel::<IndexUpdateMsg>(1000);
    RUNTIME.spawn(run_index_update_consumer(scoped_rx));
    RUNTIME.spawn(run_index_update_consumer(unscoped_rx));
    IndexUpdateChannels { scoped, unscoped }
});

async fn run_index_update_consumer(mut rx: Receiver<IndexUpdateMsg>) {
    while let Some(msg) = rx.recv().await {
        // Guard ensures decrement happens on drop (scope exit or panic).
        let _guard = TaskGuard {
            pending_counter: Arc::clone(&msg.pending_counter),
        };

        // Catch one update's panic so the lane continues processing later
        // durable markers instead of becoming permanently wedged.
        let future = async {
            if cfg!(debug_assertions)
                && let Ok(value) = std::env::var("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS")
                && let Ok(delay_ms) = value.parse::<u64>()
                && delay_ms > 0
            {
                tokio::time::sleep(Duration::from_millis(delay_ms.min(30_000))).await;
            }
            match apply_queued_index_update(&msg).await {
                Ok(()) => {}
                Err(error) => {
                    // ClientStorage::put registers the marker before queueing, so
                    // terminal failure remains replayable after the foreground
                    // command has already advanced refs or printed success.
                    record_index_update_failure(&msg);
                    tracing::warn!("Failed to update object index for {}: {}", msg.hash, error);
                }
            }
        };
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;

        if let Err(payload) = result {
            record_index_update_failure(&msg);
            tracing::error!("Panic in background index update task: {:?}", payload);
        }
    }
}

async fn apply_queued_index_update(msg: &IndexUpdateMsg) -> Result<(), String> {
    let db_path = msg.db_path.clone();
    let generation_lock =
        tokio::task::spawn_blocking(move || acquire_index_repair_generation_lock(&db_path))
            .await
            .map_err(|error| {
                format!(
                    "object-index repair generation task failed for {}: {error}",
                    msg.hash
                )
            })?
            .map_err(|error| {
                format!(
                    "failed to acquire object-index repair generation for {}: {error}",
                    msg.hash
                )
            })?;
    let _ownership = if let Some(marker_path) = msg.marker_path.as_deref() {
        let db_path = msg.db_path.clone();
        let oid = msg.hash.clone();
        let lock = tokio::task::spawn_blocking(move || acquire_index_repair_lock(&db_path, &oid))
            .await
            .map_err(|error| {
                format!(
                    "object-index repair ownership task failed for {}: {error}",
                    msg.hash
                )
            })?
            .map_err(|error| {
                format!(
                    "failed to acquire object-index repair ownership for {}: {error}",
                    msg.hash
                )
            })?;

        match fs::symlink_metadata(marker_path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(format!(
                    "object-index repair marker is not a regular file: {}",
                    marker_path.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A replay owner already reconciled and retired this exact
                // marker while the queued writer was delayed. Skipping under
                // the same OID-shard lock is what prevents post-clean resurrection.
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect object-index repair marker '{}': {error}",
                    marker_path.display()
                ));
            }
        }
        Some(lock)
    } else {
        None
    };

    update_object_index(&msg.db_path, &msg.hash, &msg.obj_type, msg.size).await?;
    if let Some(marker_path) = msg.marker_path.as_deref() {
        retire_index_repair_marker(marker_path).map_err(|error| {
            format!(
                "object index updated for {}, but its repair marker '{}' could not be retired: {error}",
                msg.hash,
                marker_path.display()
            )
        })?;
    }
    drop(generation_lock);
    Ok(())
}

// Counter for active background tasks. Read by `wait_for_background_tasks` so the CLI
// can drain pending index updates before exiting.
static PENDING_TASKS: AtomicUsize = AtomicUsize::new(0);
// Each top-level embedded CLI invocation owns a distinct failure counter.
// Queue messages clone that counter at enqueue time, so a task that finishes
// after the 60-second foreground budget cannot charge the next invocation.
// Direct library callers outside a CLI invocation share the fallback counter.
#[derive(Clone)]
struct IndexWorkScope {
    failures: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
}

impl IndexWorkScope {
    fn new() -> Self {
        Self {
            failures: Arc::new(AtomicUsize::new(0)),
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }
}

static UNSCOPED_INDEX_WORK_SCOPE: Lazy<IndexWorkScope> = Lazy::new(IndexWorkScope::new);

tokio::task_local! {
    static ACTIVE_INDEX_WORK_SCOPE: IndexWorkScope;
}

pub(crate) struct BackgroundIndexFailureScope {
    scope: IndexWorkScope,
}

impl BackgroundIndexFailureScope {
    pub(crate) fn failure_count(&self) -> usize {
        self.scope.failures.load(Ordering::SeqCst)
    }
}

// Object-index updates run behind foreground repository writes. SQLite can keep
// the repository database locked for longer than a single short busy timeout, so
// cloud backup correctness depends on retrying instead of silently dropping rows.
const INDEX_UPDATE_MAX_ATTEMPTS: usize = 12;

/// Synchronous facade for the configured object backend.
///
/// Coarse classification of an object read failure (see
/// [`ClientStorage::classify_read_failure`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectReadFailure {
    /// The object does not exist in any reachable tier.
    Missing,
    /// The object exists but its bytes are invalid (header/zlib/size).
    Corrupt,
    /// The object exists remotely but the read policy forbids fetching it.
    Unavailable,
    /// A bounded read refused the object because it exceeds the limit.
    TooLarge,
    /// Any other failure (I/O, runtime bridge, ...).
    Other,
}

/// Wraps a `dyn Storage` (local, remote, or tiered) and adapts every operation to a
/// blocking call by routing through the dedicated [`RUNTIME`]. Cheap to clone —
/// internally it is an `Arc` plus a `PathBuf`.
#[derive(Clone)]
pub struct ClientStorage {
    storage: Arc<dyn Storage>,
    base_path: PathBuf, // Keep base_path for legacy access if needed
}

/// Default tiered-storage small/large object threshold (1 MiB): objects at or
/// above this size are LRU-cached rather than stored permanently locally.
pub const DEFAULT_STORAGE_THRESHOLD_BYTES: usize = 1024 * 1024;
/// Default local LRU disk budget for large cached objects (200 MiB).
pub const DEFAULT_CACHE_SIZE_BYTES: usize = 200 * 1024 * 1024;
/// Operator command shown when an old binary sees a newer global config schema.
pub const INSTALL_NEWER_LIBRA_COMMAND: &str =
    "curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh";

static GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
const REMOTE_STORAGE_ENV_KEYS_AFTER_TYPE: &[&str] = &[
    "LIBRA_STORAGE_BUCKET",
    "LIBRA_STORAGE_ENDPOINT",
    "LIBRA_STORAGE_REGION",
    "LIBRA_STORAGE_ACCESS_KEY",
    "LIBRA_STORAGE_SECRET_KEY",
    "LIBRA_STORAGE_ALLOW_HTTP",
    "LIBRA_STORAGE_THRESHOLD",
    "LIBRA_STORAGE_CACHE_SIZE",
];

/// Typed description of a global config DB that this binary cannot safely read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfigSchemaFuture {
    pub db_path: PathBuf,
    pub current_version: i64,
    pub latest_version: Option<i64>,
}

impl GlobalConfigSchemaFuture {
    pub fn latest_supported_display(&self) -> String {
        self.latest_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "none".to_string())
    }

    pub fn binary_path_display() -> String {
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| format!("unknown ({err})"))
    }

    pub fn diagnostic_message(&self, action: &str) -> String {
        format!(
            "global config database schema is newer than this Libra binary supports; binary: {}; version: {}; config database: {}; config schema version: {}; latest supported schema version: {}; {action}; update with: {INSTALL_NEWER_LIBRA_COMMAND}",
            Self::binary_path_display(),
            env!("CARGO_PKG_VERSION"),
            self.db_path.display(),
            self.current_version,
            self.latest_supported_display(),
        )
    }
}

impl std::fmt::Display for GlobalConfigSchemaFuture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "global config database '{}' schema version {} is newer than this Libra binary supports (latest supported: {})",
            self.db_path.display(),
            self.current_version,
            self.latest_supported_display()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StorageConfigResolutionError {
    GlobalSchemaFuture(GlobalConfigSchemaFuture),
    Other(String),
}

impl std::fmt::Display for StorageConfigResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlobalSchemaFuture(future) => future.fmt(f),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// Warn once when the global config DB is too new but the current command may
/// continue in an explicit local/offline or config-irrelevant mode.
pub fn emit_global_config_schema_future_warning(future: &GlobalConfigSchemaFuture, action: &str) {
    if GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        crate::utils::error::emit_warning(future.diagnostic_message(action));
    }
}

pub(crate) fn reset_global_config_schema_future_warning_for_invocation() {
    GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED.store(false, Ordering::Relaxed);
}

/// Whether resolving the storage config for this process could fall through
/// to the global config DB. Used by the P0-12 dispatch guard to decide if a
/// remote-facing command must fail closed on a future-schema global store.
///
/// Fail-closed bias: an error while reading the process/repo-local scopes
/// means we cannot PROVE resolution stops before the global scope, so this
/// answers `true` (the guard errs on the side of failing the remote command
/// closed) instead of letting an unreadable local store silently bypass the
/// guard and degrade to local storage.
pub async fn storage_config_resolution_may_read_global_config() -> bool {
    let storage_type = match resolve_env_for_storage_init_without_global("LIBRA_STORAGE_TYPE").await
    {
        Ok(Some(storage_type)) => storage_type,
        Ok(None) => return true,
        Err(_) => return true,
    };
    match storage_type.as_str() {
        "s3" | "r2" => {
            for name in REMOTE_STORAGE_ENV_KEYS_AFTER_TYPE {
                match resolve_env_for_storage_init_without_global(name).await {
                    Ok(Some(_)) => {}
                    Ok(None) => return true,
                    Err(_) => return true,
                }
            }
            false
        }
        _ => false,
    }
}

/// See [`storage_config_resolution_may_read_global_config`] — same
/// fail-closed bias on local-scope read errors.
pub async fn env_resolution_may_read_global_config(names: &[&str]) -> bool {
    for name in names {
        match resolve_env_for_storage_init_without_global(name).await {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return true,
        }
    }
    false
}

/// The resolved tiered-storage / LRU-cache tunables (lore.md §0.10). Exposes the
/// existing `LIBRA_STORAGE_*` knobs for inspection via `libra cache info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    /// The RAW `LIBRA_STORAGE_TYPE` value (`local` only when unset), e.g.
    /// `s3`/`r2`. Not normalized — a wrong-case `R2` is reported verbatim (and
    /// `tiered` is false), matching how the backend interprets it.
    pub storage_type: String,
    /// Whether the static config selects a durable tier: a case-sensitive
    /// `s3`/`r2` `storage_type` that also passes every static fallback check the
    /// backend applies before connecting (non-empty bucket, parseable endpoint
    /// URL, non-empty access/secret key). The cache tunables only take effect
    /// when tiered. NB: an actual connection additionally requires valid
    /// credentials, which this static report does not validate.
    pub tiered: bool,
    /// Small/large object threshold in bytes (`LIBRA_STORAGE_THRESHOLD`).
    pub threshold_bytes: usize,
    /// Local LRU disk budget in bytes (`LIBRA_STORAGE_CACHE_SIZE`).
    pub cache_size_bytes: usize,
}

/// Resolve the cache/storage tunables the way [`ClientStorage::create_storage_backend`]
/// does (env first, then the global config DB via `resolve_env_sync`), mirroring
/// its lenient parse — an unparseable numeric value falls back to the default,
/// exactly as the storage backend would use it. Used by `libra cache info` so
/// the reported values match what the running backend applies.
///
/// # Errors
/// Propagates a config-resolution failure (e.g. an unreadable global config DB).
/// Whether the S3/R2 static pre-connection checks pass, resolved in the SAME
/// order as [`ClientStorage::create_storage_backend`] and short-circuiting to
/// `false` at the first static fallback (empty bucket / unparseable endpoint /
/// empty access or secret key). Each var is resolved in order so a
/// config-resolution error only surfaces for a var the backend would actually
/// have reached — `tiered` is thus never over-reported. `REGION`/`ALLOW_HTTP`
/// values do not gate tiering (the backend accepts any), but a resolution error
/// on either still degrades the backend to local, so they are resolved too.
fn tiered_static_checks_pass() -> Result<bool, String> {
    if resolve_env_sync("LIBRA_STORAGE_BUCKET")?.is_some_and(|bucket| bucket.is_empty()) {
        return Ok(false);
    }
    if let Some(endpoint) = resolve_env_sync("LIBRA_STORAGE_ENDPOINT")?
        && url::Url::parse(&endpoint).is_err()
    {
        return Ok(false);
    }
    let _region = resolve_env_sync("LIBRA_STORAGE_REGION")?;
    if resolve_env_sync("LIBRA_STORAGE_ACCESS_KEY")?.is_some_and(|key| key.is_empty()) {
        return Ok(false);
    }
    if resolve_env_sync("LIBRA_STORAGE_SECRET_KEY")?.is_some_and(|secret| secret.is_empty()) {
        return Ok(false);
    }
    let _allow_http = resolve_env_sync("LIBRA_STORAGE_ALLOW_HTTP")?;
    Ok(true)
}

pub fn resolve_cache_config() -> Result<CacheConfig, String> {
    let (storage_type, mut tiered) = match resolve_env_sync("LIBRA_STORAGE_TYPE")? {
        // Raw, case-sensitive match — identical to create_storage_backend, so a
        // value the backend rejects (e.g. `R2`, `" r2 "`) reports non-tiered
        // rather than misleading the user into thinking tiering is active.
        Some(raw) => {
            let tiered = matches!(raw.as_str(), "s3" | "r2");
            (raw, tiered)
        }
        None => ("local".to_string(), false),
    };
    // Mirror every static pre-connection fallback the backend applies, in the
    // SAME order, so `tiered` is never over-reported. (An actual connection
    // additionally needs valid credentials, which a static report cannot verify.)
    if tiered {
        tiered = tiered_static_checks_pass()?;
    }

    // Raw `.parse()` (no trim) mirrors the backend exactly: an unparseable value
    // like `" 2048 "` falls back to the default, just as the backend applies it.
    let threshold_bytes = match resolve_env_sync("LIBRA_STORAGE_THRESHOLD")? {
        Some(raw) => raw.parse().unwrap_or(DEFAULT_STORAGE_THRESHOLD_BYTES),
        None => DEFAULT_STORAGE_THRESHOLD_BYTES,
    };
    let cache_size_bytes = match resolve_env_sync("LIBRA_STORAGE_CACHE_SIZE")? {
        Some(raw) => raw.parse().unwrap_or(DEFAULT_CACHE_SIZE_BYTES),
        None => DEFAULT_CACHE_SIZE_BYTES,
    };

    Ok(CacheConfig {
        storage_type,
        tiered,
        threshold_bytes,
        cache_size_bytes,
    })
}

impl ClientStorage {
    /// Evict verified-durable large objects until under budget (lore.md
    /// 2.9). `Ok(None)` when the backing store is not tiered.
    pub async fn evict_local(
        &self,
        request: crate::utils::storage::EvictRequest,
    ) -> Result<Option<crate::utils::storage::EvictReport>, git_internal::errors::GitError> {
        self.storage.evict_local(request).await
    }

    pub fn base_path(&self) -> &PathBuf {
        &self.base_path
    }

    /// Construct a `ClientStorage` rooted at `base_path` (typically `.libra/objects`).
    ///
    /// Functional scope:
    /// - Picks the storage backend based on env / vault config (see
    ///   [`Self::create_storage_backend`]). Local-only when `LIBRA_STORAGE_TYPE` is
    ///   absent.
    ///
    /// Boundary conditions:
    /// - Never panics on misconfiguration: any unrecoverable env error degrades to
    ///   `LocalStorage` with a one-line error written to stderr. This means a broken
    ///   `LIBRA_STORAGE_*` setting silently disables remote backup instead of stopping
    ///   the CLI.
    pub fn init(base_path: PathBuf) -> ClientStorage {
        let storage = Self::create_storage_backend(base_path.clone());
        ClientStorage { storage, base_path }
    }

    /// Construct a strictly **local** `ClientStorage` rooted at `base_path`,
    /// ignoring `LIBRA_STORAGE_TYPE` and any cloud configuration.
    ///
    /// Use this when reading a *foreign* object store (for example another
    /// repository's `objects` directory): the tiered backend would otherwise
    /// fall back to the configured remote on a miss and could write fetched
    /// objects back into that foreign directory using cloud credentials.
    pub fn init_local(base_path: PathBuf) -> ClientStorage {
        let storage = Arc::new(LocalStorage::new(base_path.clone()));
        ClientStorage { storage, base_path }
    }

    /// Construct a local backend without touching the filesystem.
    ///
    /// Historical import uses this after consent: all repository object I/O
    /// is delegated to deadline-bound helpers, so even object-directory
    /// creation must not occur synchronously on the foreground thread.
    pub fn init_local_existing(base_path: PathBuf) -> ClientStorage {
        let storage = Arc::new(LocalStorage::open_no_create(base_path.clone()));
        ClientStorage { storage, base_path }
    }

    #[cfg(test)]
    pub(crate) fn from_test_storage(storage: Arc<dyn Storage>, base_path: PathBuf) -> Self {
        Self { storage, base_path }
    }

    /// Create a storage backend.
    ///
    /// # Remote Storage
    /// If `LIBRA_STORAGE_TYPE` is set to "s3" or "r2", it configures a tiered storage
    /// with local cache and remote persistence.
    ///
    /// ## Repo ID Isolation
    /// When remote storage is enabled, it attempts to read `libra.repoid` from the configuration.
    /// If found, it uses `repo_id` as a key prefix (`<repo_id>/objects/...`) for isolation.
    /// If not found (e.g., during init before config exists), it defaults to no prefix (root of bucket),
    /// which might be risky for multi-tenant buckets but acceptable for single-repo buckets.
    ///
    /// Boundary conditions:
    /// - Any env-var resolution error degrades to `LocalStorage` (see
    ///   [`Self::storage_config_resolution_fallback`]); the user sees the failure on
    ///   stderr but the CLI continues.
    /// - An empty bucket / access key / secret key, or a non-URL endpoint, also
    ///   triggers a degrade-to-local with an error message.
    /// - Unknown `LIBRA_STORAGE_TYPE` values (anything other than `s3`/`r2`) print
    ///   "Unsupported storage type" and degrade to local.
    /// - `LIBRA_STORAGE_THRESHOLD` and `LIBRA_STORAGE_CACHE_SIZE` accept any
    ///   parseable usize and silently fall back to defaults (1 MiB, 200 MiB) when the
    ///   value is not a valid number.
    /// - The `expect("Failed to build S3 storage")` is the one panicking path: it
    ///   only fires if the partial AWS builder is missing a required field, which
    ///   should be impossible given the explicit checks above.
    fn create_storage_backend(base_path: PathBuf) -> Arc<dyn Storage> {
        // Check for object storage configuration.
        // Uses the typed resolver so vault-stored secrets are picked up without
        // turning a too-new global config schema into a silent local fallback.
        let storage_type = match resolve_env_sync_typed("LIBRA_STORAGE_TYPE") {
            Ok(Some(storage_type)) => storage_type,
            Ok(None) => {
                return Arc::new(LocalStorage::new_with_alternates(base_path));
            }
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_TYPE",
                    &err,
                );
            }
        };

        let bucket = match resolve_env_sync_typed("LIBRA_STORAGE_BUCKET") {
            Ok(Some(bucket)) => bucket,
            Ok(None) => "libra".to_string(),
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_BUCKET",
                    &err,
                );
            }
        };
        if bucket.is_empty() {
            eprintln!(
                "Warning: LIBRA_STORAGE_BUCKET cannot be empty. Falling back to local storage."
            );
            return Arc::new(LocalStorage::new_with_alternates(base_path));
        }

        // Build ObjectStore
        let object_store: Arc<dyn object_store::ObjectStore> = match storage_type.as_str() {
            "s3" | "r2" => {
                let mut builder =
                    object_store::aws::AmazonS3Builder::new().with_bucket_name(&bucket);

                // Bound object_store's built-in retry (which already backs off on
                // 429/`SlowDown`/5xx and honours `Retry-After`) to the same caps
                // as `utils::backoff::RetryPolicy`, so no remote path can hammer
                // the backend or hang unbounded. See `docs/development/gap/lore.md`
                // §0.2 / §7.6.
                builder = builder.with_retry(object_store::RetryConfig {
                    backoff: object_store::BackoffConfig {
                        init_backoff: Duration::from_millis(200),
                        max_backoff: Duration::from_secs(10),
                        base: 2.0,
                    },
                    max_retries: 5,
                    retry_timeout: Duration::from_secs(60),
                });

                let endpoint = match resolve_env_sync_typed("LIBRA_STORAGE_ENDPOINT") {
                    Ok(endpoint) => endpoint,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ENDPOINT",
                            &err,
                        );
                    }
                };
                if let Some(endpoint) = endpoint {
                    if url::Url::parse(&endpoint).is_err() {
                        eprintln!(
                            "Warning: Invalid LIBRA_STORAGE_ENDPOINT URL: {}. Falling back to local storage.",
                            endpoint
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_endpoint(endpoint);
                }
                let region = match resolve_env_sync_typed("LIBRA_STORAGE_REGION") {
                    Ok(region) => region,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_REGION",
                            &err,
                        );
                    }
                };
                if let Some(region) = region {
                    builder = builder.with_region(region);
                }
                let key = match resolve_env_sync_typed("LIBRA_STORAGE_ACCESS_KEY") {
                    Ok(key) => key,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ACCESS_KEY",
                            &err,
                        );
                    }
                };
                if let Some(key) = key {
                    if key.is_empty() {
                        eprintln!(
                            "Warning: LIBRA_STORAGE_ACCESS_KEY cannot be empty. Falling back to local storage."
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_access_key_id(key);
                }
                let secret = match resolve_env_sync_typed("LIBRA_STORAGE_SECRET_KEY") {
                    Ok(secret) => secret,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_SECRET_KEY",
                            &err,
                        );
                    }
                };
                if let Some(secret) = secret {
                    if secret.is_empty() {
                        eprintln!(
                            "Warning: LIBRA_STORAGE_SECRET_KEY cannot be empty. Falling back to local storage."
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_secret_access_key(secret);
                }

                let allow_http = match resolve_env_sync_typed("LIBRA_STORAGE_ALLOW_HTTP") {
                    Ok(allow_http) => allow_http,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ALLOW_HTTP",
                            &err,
                        );
                    }
                };
                if allow_http.as_deref() == Some("true") {
                    builder = builder.with_allow_http(true);
                }

                Arc::new(builder.build().unwrap_or_else(|err| {
                    panic!(
                        "ClientStorage::with_remote: failed to build S3 storage with endpoint/\
                         bucket/region/credentials from LIBRA_STORAGE_* env: {err}"
                    )
                }))
            }
            _ => {
                eprintln!(
                    "Warning: Unsupported storage type: {}. Falling back to local storage.",
                    storage_type
                );
                return Arc::new(LocalStorage::new_with_alternates(base_path));
            }
        };

        let remote = match get_or_create_repo_id_for_prefix() {
            Some(repo_id) => RemoteStorage::new_with_prefix(object_store, repo_id),
            None => RemoteStorage::new(object_store),
        };
        let local = LocalStorage::new_with_alternates(base_path.clone());

        let threshold = match resolve_env_sync_typed("LIBRA_STORAGE_THRESHOLD") {
            Ok(Some(raw_threshold)) => raw_threshold
                .parse()
                .unwrap_or(DEFAULT_STORAGE_THRESHOLD_BYTES),
            Ok(None) => DEFAULT_STORAGE_THRESHOLD_BYTES,
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_THRESHOLD",
                    &err,
                );
            }
        };

        // Parse cache size (previously hardcoded/magic number)
        let disk_cache_limit_bytes = match resolve_env_sync_typed("LIBRA_STORAGE_CACHE_SIZE") {
            Ok(Some(raw_size)) => raw_size.parse().unwrap_or(DEFAULT_CACHE_SIZE_BYTES),
            Ok(None) => DEFAULT_CACHE_SIZE_BYTES,
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_CACHE_SIZE",
                    &err,
                );
            }
        };

        Arc::new(TieredStorage::new(
            local,
            remote,
            threshold,
            disk_cache_limit_bytes,
        ))
    }

    /// Emit a stderr warning and degrade to `LocalStorage` when a storage env
    /// var cannot be resolved. Centralised so every fallback prints the same
    /// message shape and ensures CLI commands keep working when remote storage
    /// is broken — the `Warning:` prefix mirrors the recovered, non-fatal
    /// nature of the degrade path so users do not mistake it for a fatal
    /// command failure (e.g. a `~/.libra/config.db` whose schema is newer than
    /// this binary supports still surfaces a chain like "Repository database
    /// schema version ... is newer than this Libra binary supports", but the
    /// clone/init operation itself still succeeds via `LocalStorage`).
    fn storage_config_resolution_fallback(
        base_path: &Path,
        name: &str,
        error: &StorageConfigResolutionError,
    ) -> Arc<dyn Storage> {
        if let StorageConfigResolutionError::GlobalSchemaFuture(future) = error {
            emit_global_config_schema_future_warning(
                future,
                "ignoring global storage config and falling back to local storage",
            );
            return Arc::new(LocalStorage::new_with_alternates(base_path.to_path_buf()));
        }
        eprintln!(
            "Warning: failed to resolve {}: {}. Falling back to local storage.",
            name, error
        );
        Arc::new(LocalStorage::new_with_alternates(base_path.to_path_buf()))
    }

    /// Helper to execute async task on dedicated runtime and block waiting for result.
    ///
    /// Functional scope:
    /// - Spawns `future` on the private [`RUNTIME`] and blocks the calling thread on
    ///   an `mpsc::channel` until the result is delivered.
    ///
    /// Boundary conditions:
    /// - Panics if the runtime drops the future before sending a result (e.g. runtime
    ///   shutdown) — this indicates a programmer error since RUNTIME is a `Lazy`
    ///   static and should outlive the process.
    /// - Safe to call from inside another tokio runtime: the work runs on RUNTIME, not
    ///   the caller's runtime, so nested-runtime panics are avoided.
    fn block_on_storage<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        RUNTIME.spawn(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        // INVARIANT: the spawned task above always either returns or panics
        // before exiting. recv() therefore only returns Err if the spawned
        // task panicked (sender dropped before sending). The function's doc
        // comment already documents this as a programmer error since
        // RUNTIME is a `Lazy` static that outlives the process.
        rx.recv()
            .expect("ClientStorage storage-runtime task panicked before sending result")
    }

    /// Wait for all background tasks (e.g. indexing) to complete.
    ///
    /// Functional scope:
    /// - Polls [`PENDING_TASKS`] every 100 ms until it reaches zero; logs a progress
    ///   line every 5 s so a stuck index update is visible to the user.
    ///
    /// Boundary conditions:
    /// - Has no upper time bound. If the consumer is wedged the call blocks forever;
    ///   in practice the only path that can wedge is a SQLite lock contention bug,
    ///   which the consumer's panic catcher and short busy timeouts already mitigate.
    /// - Called by the top-level CLI dispatcher just before process exit so queued
    ///   index updates are not killed mid-write.
    pub fn wait_for_background_tasks() {
        // Wait until all tasks finish
        let mut waited = 0;
        loop {
            let pending = PENDING_TASKS.load(Ordering::Acquire);
            if pending == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            waited += 100;
            if waited >= 5000 {
                tracing::info!("Waiting for {} background tasks to complete...", pending);
                waited = 0;
            }
        }
    }

    /// Asynchronously wait for the object-index queue without allowing a
    /// foreground operation to block forever behind a wedged consumer.
    pub async fn wait_for_background_tasks_until(deadline: Instant) -> bool {
        let pending_counter = current_index_pending_counter();
        loop {
            if pending_counter.load(Ordering::Acquire) == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            tokio::time::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(25)),
            )
            .await;
        }
    }

    /// Start invocation-local attribution for subsequently enqueued index work.
    /// The CLI serializes invocations, while each queued message retains this
    /// counter independently if it outlives the foreground drain budget.
    pub(crate) fn begin_background_index_failure_scope() -> BackgroundIndexFailureScope {
        BackgroundIndexFailureScope {
            scope: current_index_work_scope(),
        }
    }

    /// Run one top-level embedded CLI invocation with isolated background
    /// index failure and pending-work attribution. Tokio task locals do not
    /// leak into concurrent direct storage callers or independently spawned
    /// tasks, unlike a process-global "active invocation" slot.
    pub(crate) async fn with_background_index_failure_scope<F>(future: F) -> F::Output
    where
        F: std::future::Future,
    {
        ACTIVE_INDEX_WORK_SCOPE
            .scope(IndexWorkScope::new(), future)
            .await
    }

    /// Spawn command-owned work that may enqueue object-index updates.
    ///
    /// Tokio task-local state is not inherited by `tokio::spawn`. Registering
    /// the producer before spawning prevents the foreground drain from seeing
    /// a transient zero, and re-entering the captured scope keeps both pending
    /// work and terminal failures attributed to the command that created it.
    pub(crate) fn spawn_background_index_work<F>(future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let scope = current_index_work_scope();
        let pending_guard = register_pending_index_work(&scope);
        tokio::spawn(async move {
            let _pending_guard = pending_guard;
            ACTIVE_INDEX_WORK_SCOPE.scope(scope, future).await
        })
    }

    /// Counter for terminal background index errors attributed to the active
    /// top-level CLI invocation (or the process-wide fallback for direct
    /// library callers). Queued messages retain the counter that was active
    /// when they were created.
    pub(crate) fn background_index_failure_count() -> usize {
        current_index_failure_counter().load(Ordering::SeqCst)
    }

    /// Replay durable object-index updates left by terminal background failures.
    ///
    /// Schema-aware CLI preflight calls this before every ordinary repository
    /// command. `cloud sync` treats an error as fatal so it cannot upload from an
    /// incomplete catalogue; other commands may continue after a recorded warning.
    pub(crate) async fn repair_pending_object_index_updates(
        db_path: &Path,
    ) -> Result<ObjectIndexRepairOutcome, String> {
        let db_path = db_path.to_path_buf();
        let db_path_str = db_path.to_str().ok_or_else(|| {
            format!(
                "database path is not valid UTF-8 for object index repair: {}",
                db_path.display()
            )
        })?;
        let db_conn =
            establish_connection_with_busy_timeout(db_path_str, Duration::from_millis(200))
                .await
                .map_err(|error| {
                    format!(
                        "failed to connect to object index database {} for repair: {error}",
                        db_path.display()
                    )
                })?;
        let expected_oid_len = expected_index_repair_oid_len(&db_conn).await?;
        let load_path = db_path.clone();
        let page = tokio::task::spawn_blocking(move || {
            load_pending_object_index_updates(&load_path, expected_oid_len)
        })
        .await
        .map_err(|error| format!("object-index repair marker reader failed: {error}"))?
        .map_err(|error| format!("failed to read object-index repair markers: {error}"))?;

        if page.updates.is_empty() {
            return Ok(ObjectIndexRepairOutcome {
                repaired: 0,
                remaining: page.has_more,
            });
        }
        if cfg!(debug_assertions)
            && std::env::var_os("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL").is_some()
        {
            return Err("injected object index update failure".to_string());
        }
        let repo_id = resolve_repo_id_for_index(&db_conn).await?;
        let mut repaired = 0;
        for batch in page.updates.chunks(INDEX_REPAIR_BATCH_SIZE) {
            update_object_index_batch(&db_conn, &db_path, &repo_id, batch).await?;
            for msg in batch {
                if let Some(marker_path) = msg.marker_path.as_deref() {
                    retire_index_repair_marker(marker_path).map_err(|error| {
                        format!(
                            "updated object index for {}, but failed to retire repair marker '{}': {error}",
                            msg.hash,
                            marker_path.display()
                        )
                    })?;
                }
                repaired += 1;
            }
        }
        Ok(ObjectIndexRepairOutcome {
            repaired,
            remaining: page.has_more,
        })
    }

    /// Read a Git object's *raw payload* by its hash.
    ///
    /// Functional scope:
    /// - Returns the object content only; the `ObjectType` is dropped here. Use
    ///   [`Self::get_object_type`] when the type is needed.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::ObjectNotFound` when neither local cache nor remote
    ///   bucket holds the object.
    /// - Blocks the calling thread on the storage runtime; safe to call from sync or
    ///   async contexts.
    pub fn get(&self, object_id: &ObjectHash) -> Result<Vec<u8>, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move { storage.get(&hash).await.map(|(data, _)| data) })
    }

    /// Read an object only when the backend can enforce `limit` before
    /// materializing its payload.
    pub fn get_with_limit(&self, object_id: &ObjectHash, limit: u64) -> Result<Vec<u8>, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move {
            storage
                .get_with_limit(&hash, limit)
                .await
                .map(|(data, _)| data)
        })
    }

    /// Coarsely classify a read failure from [`Self::get`] /
    /// [`Self::get_with_limit`] for callers that degrade per-object (rename
    /// detection, previews) instead of failing the whole command.
    ///
    /// This lives in the storage layer, next to where the error messages are
    /// produced (`storage/local.rs`, `storage/load_cost.rs`,
    /// `storage/tiered.rs`), so the distinguishing details and this mapping
    /// evolve together; see `classify_read_failure_pins_storage_messages`.
    pub fn classify_read_failure(err: &GitError) -> ObjectReadFailure {
        match err {
            GitError::ObjectNotFound(detail) => {
                // tiered.rs: "... the offline/local read policy forbids
                // fetching it from the durable tier ..."
                if detail.contains("read policy forbids") {
                    ObjectReadFailure::Unavailable
                } else {
                    ObjectReadFailure::Missing
                }
            }
            GitError::InvalidObjectInfo(detail) => {
                // local.rs / load_cost.rs bounded reads: "... exceeds preview
                // limit of {limit} bytes".
                if detail.contains("exceeds preview limit") {
                    ObjectReadFailure::TooLarge
                } else {
                    ObjectReadFailure::Corrupt
                }
            }
            // A filesystem/transport failure reading the object STORE is an
            // availability fault, not an unknown one: the object may well
            // exist and be intact, we just cannot read it right now
            // (EACCES on a loose file, a dropped tier connection). Routing
            // it through `Other` would report an object-store problem under
            // the worktree warning family and send readers to inspect the
            // wrong subsystem.
            GitError::IOError(_) | GitError::NetworkError(_) => ObjectReadFailure::Unavailable,
            _ => ObjectReadFailure::Other,
        }
    }

    /// Compute a conservative bounded load cost without materializing its payload.
    pub fn object_size(&self, object_id: &ObjectHash) -> Result<Option<u64>, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move { storage.object_size(&hash).await })
    }

    /// Batch bounded load-cost preflight with one storage-runtime round trip.
    pub fn object_sizes(&self, object_ids: &[ObjectHash]) -> Result<Vec<Option<u64>>, GitError> {
        let storage = self.storage.clone();
        let hashes = object_ids.to_vec();
        self.block_on_storage(async move { storage.object_sizes(&hashes).await })
    }

    /// Batch bounded load-cost preflight that stops when `aggregate_limit`
    /// would be exceeded.
    pub fn object_sizes_with_total_limit(
        &self,
        object_ids: &[ObjectHash],
        aggregate_limit: u64,
    ) -> Result<Vec<Option<u64>>, GitError> {
        let storage = self.storage.clone();
        let hashes = object_ids.to_vec();
        self.block_on_storage(async move {
            storage
                .object_sizes_with_total_limit(&hashes, aggregate_limit)
                .await
        })
    }

    /// Attempt to repair a missing or corrupted object from the durable tier
    /// (`libra fsck --heal`, lore.md §0.4).
    ///
    /// Returns `Ok(true)` when the object was fetched, verified, and written
    /// locally; `Ok(false)` when there is no durable tier (local-only backend)
    /// or the object is absent from it. Never fabricates an object: only a
    /// payload that verifies against `object_id` is persisted. See
    /// [`crate::utils::storage::Storage::heal`].
    pub fn heal(&self, object_id: &ObjectHash) -> Result<bool, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move { storage.heal(&hash).await })
    }

    /// Persist a Git object and queue a recoverable background index update.
    ///
    /// Functional scope:
    /// - Writes the object via the configured backend (synchronously, on the storage
    ///   runtime), records an atomic repair marker, then enqueues an [`IndexUpdateMsg`]
    ///   so the cloud-backup object index reflects the new entry.
    ///
    /// Boundary conditions:
    /// - If the bounded channel is full, the message is forwarded to a runtime task
    ///   that waits for capacity. A closed channel or terminal database error leaves
    ///   the marker in `<storage>/object-index-repair`; the next schema-aware repo
    ///   command retries it, and `cloud sync` refuses to proceed while repair fails.
    /// - Failure to register the marker is returned to the caller before the queue
    ///   operation, so a completed command can never lose its only repair identity.
    /// - Returns `io::Error` (instead of `GitError`) so callers using `std::io`
    ///   abstractions can propagate the error directly.
    /// - The index update is skipped silently when the database path cannot be
    ///   resolved (e.g. base_path has no parent), since some test harnesses use
    ///   non-standard layouts.
    /// - See: `test_content_store`, `background_index_update_uses_storage_database_instead_of_cwd`.
    pub fn put(
        &self,
        obj_id: &ObjectHash,
        content: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, io::Error> {
        let storage = self.storage.clone();
        let hash = *obj_id;
        let data = content.to_vec();
        let data_len = data.len();
        let hash_str = hash.to_string();
        let type_str = obj_type.to_string();

        // First, store the object
        let result = self.block_on_storage(async move {
            storage
                .put(&hash, &data, obj_type)
                .await
                .map_err(|e| io::Error::other(e.to_string()))
        })?;

        self.enqueue_stored_object_index(&hash_str, &type_str, data_len)?;

        Ok(result)
    }

    /// Register an object that is already present in the configured storage.
    /// This is the retry path for callers whose payload write succeeded but
    /// durable marker registration failed. It intentionally avoids rewriting a
    /// potentially remote payload while still recreating the exact repair work.
    pub(crate) fn ensure_existing_object_index(
        &self,
        obj_id: &ObjectHash,
        content_len: usize,
        obj_type: ObjectType,
    ) -> io::Result<()> {
        if !self.exist(obj_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "cannot register object {} in the cloud index because its payload is no longer present",
                    obj_id
                ),
            ));
        }
        self.enqueue_stored_object_index(&obj_id.to_string(), &obj_type.to_string(), content_len)
    }

    fn enqueue_stored_object_index(
        &self,
        hash_str: &str,
        type_str: &str,
        data_len: usize,
    ) -> io::Result<()> {
        // Update object index asynchronously (via sequential queue). This keeps
        // foreground writes nonblocking while the marker makes retry durable.
        if let Some(db_path) = Self::index_db_path_from_base(&self.base_path)
            && db_path.exists()
        {
            let mut msg = IndexUpdateMsg {
                hash: hash_str.to_string(),
                obj_type: type_str.to_string(),
                size: data_len as i64,
                db_path,
                marker_path: None,
                _marker_lock: None,
                failure_counter: current_index_failure_counter(),
                pending_counter: current_index_pending_counter(),
            };
            msg.marker_path = Some(persist_index_repair_marker(&msg).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "stored object {hash_str}, but failed to register its cloud object-index repair marker: {error}"
                    ),
                )
            })?);
            enqueue_index_update(msg, "object index update");
        }

        Ok(())
    }

    /// Physically delete an object's payload (lore.md 2.5) from the durable
    /// tier and the in-memory cache; a no-op for a local-only store.
    pub async fn delete_payload(&self, hash: &ObjectHash) -> Result<(), GitError> {
        self.storage.delete_payload(hash).await
    }

    /// Check whether an object exists in the configured backend.
    ///
    /// Boundary conditions:
    /// - For tiered storage, returns `true` if the object lives in either tier; does
    ///   not promote the object to the local cache.
    pub fn exist(&self, obj_id: &ObjectHash) -> bool {
        let storage = self.storage.clone();
        let hash = *obj_id;
        self.block_on_storage(async move { storage.exist(&hash).await })
    }

    /// Read just the `ObjectType` for `obj_id`.
    ///
    /// Boundary conditions:
    /// - For backends that store the object body inline with its type header, this
    ///   may decode the entire body and discard the payload. Prefer
    ///   [`Self::is_object_type`] when only checking a single type.
    pub fn get_object_type(&self, obj_id: &ObjectHash) -> Result<ObjectType, GitError> {
        let storage = self.storage.clone();
        let hash = *obj_id;
        self.block_on_storage(async move { storage.get(&hash).await.map(|(_, t)| t) })
    }

    /// Convenience wrapper: returns whether `obj_id` resolves to an object of the
    /// requested type. Returns `false` on any read error (rather than propagating)
    /// because callers typically use this in match arms where missing-or-wrong-type
    /// have the same effect.
    pub fn is_object_type(&self, obj_id: &ObjectHash, obj_type: ObjectType) -> bool {
        match self.get_object_type(obj_id) {
            Ok(t) => t == obj_type,
            Err(_) => false,
        }
    }

    /// Search for objects matching the provided revision-ish identifier.
    ///
    /// Functional scope:
    /// - Wraps [`Self::search_result`]; logs and swallows errors to keep the simple
    ///   "list of hashes" return shape that callers expect.
    ///
    /// Boundary conditions:
    /// - On any error, returns an empty vector and logs an `error!`. Use
    ///   [`Self::search_result`] when the caller needs to react to the error.
    pub async fn search(&self, obj_id: &str) -> Vec<ObjectHash> {
        match self.search_result(obj_id).await {
            Ok(matches) => matches,
            Err(error) => {
                tracing::error!("failed to search objects for '{obj_id}': {error}");
                Vec::new()
            }
        }
    }

    /// Search for objects matching `obj_id`, surfacing errors to the caller.
    ///
    /// Functional scope:
    /// - Recognises `HEAD`, branch names, and Git navigation suffixes (`~`, `^`).
    /// - For navigation forms (`HEAD~3`, `main^^`) resolves the base ref then walks
    ///   parent commits via [`Self::navigate_commit_path`].
    /// - For prefix matches (e.g. an abbreviated SHA) delegates to the underlying
    ///   storage's `search`.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(vec![])` when an empty base ref is supplied (e.g. `~1`, `^2`)
    ///   to avoid degenerating into a prefix search of all objects.
    /// - Returns `Ok(vec![])` when the base ref is ambiguous (multiple matching
    ///   commit objects). The caller decides whether ambiguity is an error.
    /// - Returns `Err` when an underlying database/branch read fails (e.g. corrupt
    ///   `reference` row), so users see the actionable error instead of silent empty.
    /// - See: `test_search_result_surfaces_corrupt_branch_storage`,
    ///   `test_search_result_rejects_empty_base_ref_navigation`.
    pub async fn search_result(&self, obj_id: &str) -> Result<Vec<ObjectHash>, GitError> {
        if obj_id == "HEAD" {
            return Ok(Head::current_commit_result()
                .await
                .map_err(|error| GitError::CustomError(format!("failed to resolve HEAD: {error}")))?
                .into_iter()
                .collect());
        }

        if obj_id.contains('~') || obj_id.contains('^') {
            // Complex navigation relies on sync object loads. This stays on the
            // current runtime thread and delegates object reads through `self.get()`,
            // which already uses the dedicated background runtime.
            let mut split_pos = 0;
            let mut found_special = false;
            for (i, c) in obj_id.char_indices() {
                if c == '~' || c == '^' {
                    found_special = true;
                    split_pos = i;
                    break;
                }
            }

            if found_special {
                let base_ref = &obj_id[..split_pos];
                let path_part = &obj_id[split_pos..];

                // Reject empty base_ref (e.g. user passes "~1" or "^2") to avoid
                // a degenerate prefix search for "" which would list all objects.
                if base_ref.is_empty() {
                    return Ok(Vec::new());
                }

                let base_commit =
                    match base_ref {
                        "HEAD" => match Head::current_commit_result().await.map_err(|error| {
                            GitError::CustomError(format!("failed to resolve HEAD: {error}"))
                        })? {
                            Some(commit) => commit,
                            None => return Ok(Vec::new()),
                        },
                        _ => match Branch::find_branch_result(base_ref, None).await.map_err(
                            |error| {
                                GitError::CustomError(format!(
                                    "failed to resolve branch '{base_ref}': {error}"
                                ))
                            },
                        )? {
                            Some(branch) => branch.commit,
                            None => {
                                if Branch::exists_result(base_ref, None)
                                    .await
                                    .map_err(|error| {
                                        GitError::CustomError(format!(
                                            "failed to resolve branch '{base_ref}': {error}"
                                        ))
                                    })?
                                {
                                    return Ok(Vec::new());
                                }

                                let matches = self.storage.search(base_ref).await;
                                let commits: Vec<ObjectHash> = matches
                                    .into_iter()
                                    .filter(|x| self.is_object_type(x, ObjectType::Commit))
                                    .collect();

                                if commits.len() == 1 {
                                    commits[0]
                                } else {
                                    return Ok(Vec::new());
                                }
                            }
                        },
                    };

                let target_commit = match self.navigate_commit_path(base_commit, path_part) {
                    Ok(commit) => commit,
                    Err(_) => return Ok(Vec::new()),
                };

                return Ok(vec![target_commit]);
            }
        }

        Ok(self.storage.search(obj_id).await)
    }

    /// Walk parent commits according to a Git revision suffix.
    ///
    /// Functional scope:
    /// - Parses every `~N` and `^N` token in `path` and walks accordingly:
    ///   `^N` selects the Nth parent of the current commit; `~N` walks N first-parent
    ///   steps.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::InvalidArgument` when `path` does not match the expected
    ///   shape at all (defensive: callers already pre-filter on `~` / `^`).
    /// - Returns `GitError::ObjectNotFound` when a requested parent index does not
    ///   exist (e.g. `~5` on a commit whose history is shorter, or `^2` on a non-merge
    ///   commit).
    /// - When the count is missing (`~` rather than `~1`) it defaults to 1, matching
    ///   Git's convention.
    fn navigate_commit_path(
        &self,
        base_commit: ObjectHash,
        path: &str,
    ) -> Result<ObjectHash, GitError> {
        let mut current = base_commit;
        // INVARIANT: compile-time literal regex with two capture groups;
        // Regex::new only fails on syntactically invalid patterns, which
        // is caught by the surrounding parent-traversal tests.
        let re =
            Regex::new(r"(\^|~)(\d*)").expect("revision-suffix regex is a valid hardcoded pattern");

        if !re.is_match(path) {
            return Err(GitError::InvalidArgument(format!(
                "Invalid reference path: {path}"
            )));
        }
        for cap in re.captures_iter(path) {
            // INVARIANT: capture group 1 is non-optional (`(\^|~)`), so any
            // match produced by `captures_iter` is guaranteed to populate it.
            let symbol = cap
                .get(1)
                .expect("regex capture group 1 is non-optional")
                .as_str();
            let num_str = cap.get(2).map_or("1", |m| m.as_str());
            let num: usize = num_str.parse().unwrap_or(1);

            match symbol {
                "^" => {
                    current = self.get_parent_commit(&current, num)?;
                }
                "~" => {
                    for _ in 0..num {
                        current = self.get_parent_commit(&current, 1)?;
                    }
                }
                // INVARIANT: regex `(\^|~)(\d*)` only captures "^" or "~" in
                // group 1, so `symbol` cannot hold any other value here.
                _ => unreachable!("regex capture group 1 is restricted to \"^\" or \"~\""),
            }
        }
        Ok(current)
    }

    /// Return the Nth parent (1-indexed) of `commit_id`.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::ObjectNotFound` when `n == 0` or `n` exceeds the parent
    ///   count. Callers using `^` semantics never pass 0; the explicit check is for
    ///   safety against future callers.
    fn get_parent_commit(&self, commit_id: &ObjectHash, n: usize) -> Result<ObjectHash, GitError> {
        let commit: Commit = load_object(commit_id)?;
        if n == 0 || n > commit.parent_commit_ids.len() {
            return Err(GitError::ObjectNotFound(format!(
                "Parent {n} does not exist"
            )));
        }
        Ok(commit.parent_commit_ids[n - 1])
    }

    /// Compress `data` with zlib using the default compression level — exposed for
    /// tests and other utilities that produce loose-object byte streams.
    pub fn compress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let compressed_data = encoder.finish()?;
        Ok(compressed_data)
    }

    /// Inverse of [`Self::compress_zlib`] — decompress a previously-zlib-compressed
    /// byte slice. Used by tests and pack inspection paths.
    pub fn decompress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoder = ZlibDecoder::new(data);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;
        Ok(decompressed_data)
    }

    /// Map `<storage>/objects` back to `<storage>/<DATABASE>` so background index
    /// updates write to the database that owns this objects directory rather than
    /// to whichever database happens to be discoverable from the process CWD.
    fn index_db_path_from_base(base_path: &Path) -> Option<PathBuf> {
        base_path
            .parent()
            .map(|storage_path| storage_path.join(DATABASE))
    }
}

/// Enqueue an `object_index` row for an object that was written outside the
/// usual `ClientStorage::put` path — currently agent capture transcript and
/// metadata blobs, which `HistoryManager::append_checkpoint_commit` writes
/// directly via [`crate::utils::object::write_git_object`] for the orphan
/// `refs/libra/traces` history.
///
/// Why this exists: cloud sync uploads only the rows it finds in
/// `object_index` — anything that bypasses `object_index` is invisible to
/// `libra cloud sync`. Without this hook, agent transcripts written by the
/// hook runtime would never reach R2, and the Phase 3.5b `cloud restore`
/// catalogue would resolve commit OIDs that pointed at missing blobs on a
/// fresh clone (entire.md §14.3 phase-3 item 3 — "走正常 R2 同步").
///
/// The function takes the `.libra` directory rather than the storage objects
/// path because agent capture callers already hold a `repo_path` shaped that
/// way; the db lives at `<libra_dir>/<DATABASE>`. Returns immediately when
/// the database file is absent so legacy bootstrap and tempdir tests stay
/// quiet. Marker persistence is a hard precondition: callers receive an error
/// and no queue item is created when durable repair ownership cannot be recorded.
///
/// `pub(crate)` — there is no validation that the (`o_id`, `o_type`,
/// `o_size`) triple matches an actual on-disk Git object, so this is an
/// internal escape hatch for agent-history/review/investigate callers that
/// already hold the truth. External crates / users must go through
/// `ClientStorage::put`, which both writes the object and indexes it.
pub(crate) fn enqueue_agent_blob_object_index_update(
    libra_dir: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) -> io::Result<()> {
    let db_path = libra_dir.join(DATABASE);
    if !db_path.exists() {
        return Ok(());
    }
    let mut msg = IndexUpdateMsg {
        hash: o_id.to_string(),
        obj_type: o_type.to_string(),
        size: o_size,
        db_path,
        marker_path: None,
        _marker_lock: None,
        failure_counter: current_index_failure_counter(),
        pending_counter: current_index_pending_counter(),
    };
    msg.marker_path = Some(persist_index_repair_marker(&msg).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "agent object {o_id} was stored, but its durable cloud object-index repair marker could not be registered: {error}"
            ),
        )
    })?);
    enqueue_index_update(msg, "agent blob object index update");
    Ok(())
}

/// Delete `object_index` rows for the given OIDs in the current repo
/// (AG-20 prune-side counterpart of
/// [`enqueue_agent_blob_object_index_update`]).
///
/// Functional scope:
/// - Resolves the repo id the same way the indexing writer does
///   (`libra.repoid` config, falling back to `unknown-repo` only when the
///   value is absent or blank) so the delete predicate matches the rows the
///   writer created.
/// - Deletes in bounded `IN (...)` chunks and returns the total number of
///   rows removed. Idempotent: OIDs without a row simply delete nothing.
///
/// Boundary conditions:
/// - Returns `Ok(0)` without touching anything when `oids` is empty or the
///   `object_index` table does not exist (minimal test databases).
/// - `pub(crate)` — callers must already have proven the OIDs unreachable
///   (only `HistoryManager::commit_checkpoint_prune` today); there is no
///   reachability validation here.
pub(crate) async fn remove_object_index_rows_with_conn<C: ConnectionTrait>(
    conn: &C,
    oids: &[String],
) -> Result<u64, DbErr> {
    if oids.is_empty() {
        return Ok(0);
    }
    let backend = conn.get_database_backend();
    let table_exists = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'object_index' LIMIT 1"
                .to_string(),
        ))
        .await?
        .is_some();
    if !table_exists {
        return Ok(0);
    }

    // Resolve the repo id exactly like the indexing writer. Query and decode
    // failures must abort the prune transaction: silently targeting the
    // `unknown-repo` sentinel would report success while leaving stale cloud
    // catalogue rows behind.
    let repo_id = match conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1"
                .to_string(),
        ))
        .await?
    {
        Some(row) => {
            let value = row.try_get_by::<String, _>("value")?;
            if value.trim().is_empty() {
                "unknown-repo".to_string()
            } else {
                value
            }
        }
        None => "unknown-repo".to_string(),
    };

    // SQLite's default host-parameter limit is generous (32k), but keep the
    // chunks small so a huge prune cannot produce pathological statements.
    const DELETE_CHUNK: usize = 200;
    let mut deleted = 0_u64;
    for chunk in oids.chunks(DELETE_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("DELETE FROM object_index WHERE repo_id = ? AND o_id IN ({placeholders})");
        let mut values: Vec<Value> = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::from(repo_id.clone()));
        values.extend(chunk.iter().map(|oid| Value::from(oid.clone())));
        let result = conn
            .execute_raw(Statement::from_sql_and_values(backend, sql, values))
            .await?;
        deleted += result.rows_affected();
    }
    Ok(deleted)
}

/// Count the `object_index` rows [`remove_object_index_rows_with_conn`]
/// WOULD delete for `oids` — the `--dry-run` preview counterpart (PD-04).
/// Mirrors the delete predicate exactly (same repo-id resolution, same
/// chunking) and shares its boundary behavior: empty input or a missing
/// `object_index` table count as zero.
pub(crate) async fn count_object_index_rows_with_conn<C: ConnectionTrait>(
    conn: &C,
    oids: &[String],
) -> Result<u64, DbErr> {
    if oids.is_empty() {
        return Ok(0);
    }
    let backend = conn.get_database_backend();
    let table_exists = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'object_index' LIMIT 1"
                .to_string(),
        ))
        .await?
        .is_some();
    if !table_exists {
        return Ok(0);
    }
    let repo_id = match conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1"
                .to_string(),
        ))
        .await?
    {
        Some(row) => {
            let value = row.try_get_by::<String, _>("value")?;
            if value.trim().is_empty() {
                "unknown-repo".to_string()
            } else {
                value
            }
        }
        None => "unknown-repo".to_string(),
    };
    const COUNT_CHUNK: usize = 200;
    let mut total = 0_u64;
    for chunk in oids.chunks(COUNT_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT COUNT(*) AS n FROM object_index WHERE repo_id = ? AND o_id IN ({placeholders})"
        );
        let mut values: Vec<Value> = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::from(repo_id.clone()));
        values.extend(chunk.iter().map(|oid| Value::from(oid.clone())));
        if let Some(row) = conn
            .query_one_raw(Statement::from_sql_and_values(backend, sql, values))
            .await?
        {
            total += row.try_get_by::<i64, _>("n")? as u64;
        }
    }
    Ok(total)
}

#[async_trait]
impl Storage for ClientStorage {
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
        let storage = self.storage.clone();
        let hash = *hash;
        self.block_on_storage(async move { storage.get(&hash).await })
    }

    async fn get_with_limit(
        &self,
        hash: &ObjectHash,
        limit: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        // This trait method is already async and is used by callers that wrap
        // each read in a Tokio deadline. Await the backend directly so that
        // timeout/cancellation can be polled; routing through the synchronous
        // `block_on_storage` facade would park the caller thread in recv() and
        // make an outer timeout ineffective on a blocked local/pack read.
        self.storage.get_with_limit(hash, limit).await
    }

    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError> {
        ClientStorage::put(self, hash, data, obj_type).map_err(GitError::IOError)
    }

    async fn exist(&self, hash: &ObjectHash) -> bool {
        ClientStorage::exist(self, hash)
    }

    async fn object_size(&self, hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        ClientStorage::object_size(self, hash)
    }

    async fn object_sizes(&self, hashes: &[ObjectHash]) -> Result<Vec<Option<u64>>, GitError> {
        ClientStorage::object_sizes(self, hashes)
    }

    async fn object_sizes_with_total_limit(
        &self,
        hashes: &[ObjectHash],
        aggregate_limit: u64,
    ) -> Result<Vec<Option<u64>>, GitError> {
        ClientStorage::object_sizes_with_total_limit(self, hashes, aggregate_limit)
    }

    async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
        ClientStorage::search(self, prefix).await
    }
}

/// Resolve an environment variable, checking both system env and vault config.
///
/// First checks `std::env::var` (fast, sync). If the system env var is absent,
/// it reuses the async `resolve_env()` path on a dedicated thread so local/global
/// config and vault-backed values share exactly the same semantics.
///
/// This avoids deadlocks from nested tokio runtimes during storage init, which
/// runs synchronously and may be called from within async test contexts.
///
/// Boundary conditions:
/// - Returns `Ok(None)` only when neither the system env nor any config scope
///   contains the value.
/// - Returns `Err(String)` when the worker thread crashes before sending or when
///   the underlying config lookup raises an error (e.g. corrupt SQLite, unreadable
///   permissions). Callers convert this into a hard storage configuration failure
///   rather than silently degrading.
fn resolve_env_sync(name: &str) -> Result<Option<String>, String> {
    resolve_env_sync_typed(name).map_err(|err| err.to_string())
}

fn resolve_env_sync_typed(name: &str) -> Result<Option<String>, StorageConfigResolutionError> {
    // Always check system environment first.
    if let Ok(val) = std::env::var(name) {
        return Ok(Some(val));
    }

    let owned = name.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = resolve_env_sync_worker(&owned);
        let _ = tx.send(result);
    });
    match rx.recv() {
        Ok(result) => result,
        Err(_) => Err(StorageConfigResolutionError::Other(format!(
            "env resolution worker for '{name}' exited before returning a result"
        ))),
    }
}

/// Worker side of [`resolve_env_sync`]: builds a single-purpose tokio runtime in a
/// dedicated thread so we can drive the async config lookup without colliding with
/// any runtime the caller already owns.
fn resolve_env_sync_worker(name: &str) -> Result<Option<String>, StorageConfigResolutionError> {
    let runtime = tokio::runtime::Runtime::new().map_err(|err| {
        StorageConfigResolutionError::Other(format!(
            "failed to create tokio runtime for env resolution of '{name}': {err}"
        ))
    })?;
    runtime.block_on(resolve_env_for_storage_init_typed(name))
}

/// Look up `name` in the local repo's config first, then in the global config.
///
/// Functional scope:
/// - Reads `vault.env.<name>` from `<repo>/.libra/<DATABASE>` if it exists, then from
///   the global config (overridable via `LIBRA_CONFIG_GLOBAL_DB`).
///
/// Boundary conditions:
/// - Returns `Ok(None)` when neither database holds the key.
/// - Returns `Err` when a database file exists but cannot be opened or queried — the
///   caller surfaces this so the user sees actionable errors rather than silently
///   degrading to local-only storage on a typo'd schema.
async fn resolve_env_for_storage_init_typed(
    name: &str,
) -> Result<Option<String>, StorageConfigResolutionError> {
    if let Some(value) = resolve_env_for_storage_init_without_global(name).await? {
        return Ok(Some(value));
    }

    let vault_key = format!("vault.env.{name}");

    if let Some(global_db_path) = storage_global_config_path()
        && global_db_path.exists()
    {
        if let Some(future) = inspect_global_config_schema_future_at_path(&global_db_path).await {
            return Err(StorageConfigResolutionError::GlobalSchemaFuture(future));
        }
        match read_config_env_value(name, &vault_key, &global_db_path, "global")
            .await
            .map_err(StorageConfigResolutionError::Other)
        {
            Ok(Some(value)) => return Ok(Some(value)),
            Ok(None) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(None)
}

async fn resolve_env_for_storage_init_without_global(
    name: &str,
) -> Result<Option<String>, StorageConfigResolutionError> {
    if let Ok(val) = std::env::var(name) {
        return Ok(Some(val));
    }

    let vault_key = format!("vault.env.{name}");

    if let Ok(storage_path) = try_get_storage_path(None) {
        let local_db_path = storage_path.join(DATABASE);
        if local_db_path.exists()
            && let Some(value) = read_config_env_value(name, &vault_key, &local_db_path, "local")
                .await
                .map_err(StorageConfigResolutionError::Other)?
        {
            return Ok(Some(value));
        }
    }

    Ok(None)
}

/// Inspect the configured global config DB and return only the too-new-schema
/// case. Other config errors are left to the normal resolver path so commands
/// that never touch global storage config keep their historical behavior.
pub async fn inspect_global_config_schema_future() -> Option<GlobalConfigSchemaFuture> {
    let global_db_path = storage_global_config_path()?;
    if !global_db_path.exists() {
        return None;
    }
    inspect_global_config_schema_future_at_path(&global_db_path).await
}

pub(crate) async fn inspect_global_config_schema_future_at_path(
    global_db_path: &Path,
) -> Option<GlobalConfigSchemaFuture> {
    match db::inspect_database_schema(global_db_path).await {
        Ok(db::SchemaCompatibility::UnsupportedFuture {
            current_version,
            latest_version,
        }) => Some(GlobalConfigSchemaFuture {
            db_path: global_db_path.to_path_buf(),
            current_version,
            latest_version,
        }),
        Ok(_) | Err(_) => None,
    }
}

/// Read a single `vault.env.*` entry from a config database, decrypting if needed.
///
/// Functional scope:
/// - Connects with a 200 ms busy timeout so background storage init cannot block on
///   foreground writers.
/// - When the entry is encrypted, decrypts using the per-scope key (local repo key
///   or global key).
///
/// Boundary conditions:
/// - Returns `Err` when the database path is not valid UTF-8 (sea-orm needs a
///   string-typed URL).
/// - Returns `Err` when decryption fails — the user sees the raw vault error, not a
///   silent fall-back to plaintext.
async fn read_config_env_value(
    env_name: &str,
    vault_key: &str,
    db_path: &Path,
    scope: &str,
) -> Result<Option<String>, String> {
    let db_path_str = db_path.to_str().ok_or_else(|| {
        format!(
            "database path is not valid UTF-8 for {scope} config: {}",
            db_path.display()
        )
    })?;
    let conn = establish_connection_with_busy_timeout(db_path_str, Duration::from_millis(200))
        .await
        .map_err(|err| match scope {
            "global" => format!(
                "failed to connect to global config '{}': {}",
                db_path.display(),
                err
            ),
            _ => format!(
                "failed to connect to local config '{}': {}",
                db_path.display(),
                err
            ),
        })?;

    let entry = ConfigKv::get_with_conn(&conn, vault_key)
        .await
        .map_err(|err| format!("failed to read '{env_name}' from {scope} config: {err}"))?;

    match entry {
        Some(entry) if entry.encrypted => decrypt_value(&entry.value, scope)
            .await
            .map(Some)
            .map_err(|err| {
                if scope == "global" {
                    format!("failed to decrypt vault.env.{env_name} from global config: {err}")
                } else {
                    format!("failed to decrypt vault.env.{env_name}: {err}")
                }
            }),
        Some(entry) => Ok(Some(entry.value)),
        None => Ok(None),
    }
}

/// Locate the global config database.
///
/// Boundary conditions:
/// - Honours `LIBRA_CONFIG_GLOBAL_DB` first so tests can redirect to a temp path.
/// - Returns `None` when no home directory is discoverable; on those platforms global
///   config is unavailable.
fn storage_global_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LIBRA_CONFIG_GLOBAL_DB") {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|home| home.join(".libra").join("config.db"))
}

/// Resolve (and lazily create) the per-repo `libra.repoid` used as a key prefix in
/// shared S3/R2 buckets.
///
/// Functional scope:
/// - Reads `libra.repoid` from the local config; if missing or set to the legacy
///   placeholder `"unknown-repo"`, generates a fresh UUID and persists it so future
///   invocations stay aligned with the same prefix.
///
/// Boundary conditions:
/// - Returns `None` if there is no resolvable storage path or no database file yet
///   (`libra init` has not run); the caller falls back to no prefix in that case.
/// - The whole computation runs on RUNTIME via mpsc, mirroring the rest of this
///   module's blocking-into-async pattern.
fn get_or_create_repo_id_for_prefix() -> Option<String> {
    let storage_path = try_get_storage_path(None).ok()?;
    let db_path = storage_path.join(DATABASE);
    if !db_path.exists() {
        return None;
    }

    let (tx, rx) = mpsc::channel();
    RUNTIME.spawn(async move {
        let mut repo_id = ConfigKv::get("libra.repoid")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        let needs_init = repo_id
            .as_deref()
            .map(|s| s.is_empty() || s == "unknown-repo")
            .unwrap_or(true);
        if needs_init {
            let new_id = Uuid::new_v4().to_string();
            let _ = ConfigKv::set("libra.repoid", &new_id, false).await;
            repo_id = Some(new_id);
        }
        let _ = tx.send(repo_id);
    });

    rx.recv().ok().flatten()
}

async fn expected_index_repair_oid_len(db_conn: &DatabaseConnection) -> Result<usize, String> {
    let object_format = ConfigKv::get_with_conn(db_conn, "core.objectformat")
        .await
        .map_err(|error| {
            format!("failed to read core.objectformat for object-index repair: {error}")
        })?
        .map(|entry| entry.value)
        .unwrap_or_else(|| "sha1".to_string());
    match object_format.trim() {
        "sha1" => Ok(40),
        "sha256" => Ok(64),
        other => Err(format!(
            "unsupported core.objectformat '{other}' while validating object-index repair markers"
        )),
    }
}

fn load_pending_object_index_updates(
    db_path: &Path,
    expected_oid_len: usize,
) -> io::Result<PendingObjectIndexPage> {
    let generation_lock = acquire_index_repair_generation_lock(db_path)?;
    scavenge_index_repair_staging(db_path)?;
    let marker_dir = db_path
        .parent()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "repository database path has no storage directory: {}",
                    db_path.display()
                ),
            )
        })?
        .join(INDEX_REPAIR_MARKER_DIR);
    let entries = match fs::read_dir(&marker_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PendingObjectIndexPage {
                updates: Vec::new(),
                has_more: false,
                _generation_lock: None,
            });
        }
        Err(error) => return Err(error),
    };

    let mut marker_paths = Vec::new();
    let mut held_locks: HashMap<String, Arc<ObjectIndexRepairLock>> = HashMap::new();
    let mut has_more = false;
    for (scanned_entries, entry) in entries.enumerate() {
        let entry = entry?;
        // A replay invocation owns at most one SQLite batch. This releases all
        // OID-shard locks after each batch instead of blocking foreground
        // writers for the lifetime of a potentially 100,000-marker page.
        if marker_paths.len() >= INDEX_REPAIR_BATCH_SIZE {
            has_more = true;
            break;
        }
        // Bound raw directory enumeration, not just retained markers. The
        // extra entry is only a sentinel: validating it would turn a bounded
        // page into another full-directory scan under a very large outage.
        if scanned_entries >= INDEX_REPAIR_MARKER_PAGE_CAP {
            has_more = true;
            break;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::other(format!(
                    "object-index repair directory contains a non-UTF-8 entry: {}",
                    path.display()
                ))
            })?;
        let Some(identity) = name.strip_suffix(".json") else {
            // Current writers stage in a sibling directory, so any `.tmp*`
            // entry here is a legacy crash remnant. Remove it within the raw
            // enumeration budget; otherwise enough abandoned scratch files
            // could permanently hide every real marker behind the page cap.
            if name.starts_with(".tmp") {
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if !metadata.file_type().is_file() {
                    return Err(io::Error::other(format!(
                        "object-index repair scratch entry is not a regular file: {}",
                        path.display()
                    )));
                }
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                continue;
            }
            return Err(io::Error::other(format!(
                "unexpected entry in object-index repair directory: {}",
                path.display()
            )));
        };
        let (oid, object_type) = identity.split_once('.').ok_or_else(|| {
            io::Error::other(format!(
                "invalid object-index repair marker filename: {}",
                path.display()
            ))
        })?;
        if !valid_index_repair_oid(oid, expected_oid_len) || !valid_index_repair_type(object_type) {
            return Err(io::Error::other(format!(
                "invalid object-index repair marker filename: {}",
                path.display()
            )));
        }
        let lock_shard = index_repair_lock_shard(oid)?;
        let marker_lock = if let Some(lock) = held_locks.get(&lock_shard) {
            Arc::clone(lock)
        } else {
            let Some(lock) = try_acquire_index_repair_lock(db_path, oid)? else {
                // A queued writer or another replay owns this marker. Leave it
                // for a later bounded page rather than blocking an async CLI
                // preflight.
                has_more = true;
                continue;
            };
            let lock = Arc::new(lock);
            held_locks.insert(lock_shard, Arc::clone(&lock));
            lock
        };
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() {
            return Err(io::Error::other(format!(
                "object-index repair marker is not a regular file: {}",
                path.display()
            )));
        }
        marker_paths.push((path, marker_lock));
    }
    marker_paths.sort_by(|left, right| left.0.cmp(&right.0));

    let mut pending = Vec::with_capacity(marker_paths.len());
    for (marker_path, marker_lock) in marker_paths {
        let mut file = match fs::File::open(&marker_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        (&mut file)
            .take(INDEX_REPAIR_MARKER_READ_CAP.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > INDEX_REPAIR_MARKER_READ_CAP {
            return Err(io::Error::other(format!(
                "object-index repair marker exceeds {} bytes: {}",
                INDEX_REPAIR_MARKER_READ_CAP,
                marker_path.display()
            )));
        }
        let marker: PendingObjectIndexUpdate = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::other(format!(
                "invalid object-index repair marker '{}': {error}",
                marker_path.display()
            ))
        })?;
        let file_identity = marker_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|identity| identity.split_once('.'));
        if marker.schema_version != INDEX_REPAIR_MARKER_SCHEMA_VERSION
            || file_identity != Some((marker.o_id.as_str(), marker.o_type.as_str()))
            || !valid_index_repair_oid(&marker.o_id, expected_oid_len)
            || !valid_index_repair_type(&marker.o_type)
            || marker.o_size < 0
        {
            return Err(io::Error::other(format!(
                "object-index repair marker failed validation: {}",
                marker_path.display()
            )));
        }
        pending.push(IndexUpdateMsg {
            hash: marker.o_id,
            obj_type: marker.o_type,
            size: marker.o_size,
            db_path: db_path.to_path_buf(),
            marker_path: Some(marker_path),
            _marker_lock: Some(marker_lock),
            failure_counter: current_index_failure_counter(),
            pending_counter: current_index_pending_counter(),
        });
    }
    Ok(PendingObjectIndexPage {
        updates: pending,
        has_more,
        _generation_lock: Some(generation_lock),
    })
}

fn scavenge_index_repair_staging(db_path: &Path) -> io::Result<()> {
    let storage_dir = db_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "repository database path has no storage directory: {}",
                db_path.display()
            ),
        )
    })?;
    let staging_dir = storage_dir.join(INDEX_REPAIR_MARKER_STAGING_DIR);
    let entries = match fs::read_dir(&staging_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0_usize;
    for entry in entries.take(INDEX_REPAIR_STAGING_SCAN_CAP) {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(".tmp") {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() {
            return Err(io::Error::other(format!(
                "object-index repair staging entry is not a regular file: {}",
                path.display()
            )));
        }
        let modified = metadata.modified()?;
        if now.duration_since(modified).unwrap_or_default() < INDEX_REPAIR_STAGING_STALE_AFTER {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if removed >= INDEX_REPAIR_STAGING_REMOVE_CAP {
            break;
        }
    }
    Ok(())
}

fn valid_supported_index_repair_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_index_repair_oid(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len && valid_supported_index_repair_oid(value)
}

fn valid_index_repair_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Reconcile one bounded page of durable repair markers with a single SQLite
/// statement on an already-open connection. The marker itself is sufficient
/// provenance that the content-addressed payload write completed: ordinary
/// storage creates it only after the configured backend succeeds, and agent
/// capture creates it only after its direct local object write succeeds.
/// Payloads may subsequently be local, packed, alternate-backed, or remote-only;
/// cloud sync remains responsible for reading and validating them before upload.
async fn update_object_index_batch(
    db_conn: &DatabaseConnection,
    db_path: &Path,
    repo_id: &str,
    updates: &[IndexUpdateMsg],
) -> Result<(), String> {
    if updates.is_empty() {
        return Ok(());
    }

    let created_at = chrono::Utc::now().timestamp();
    let mut sql = String::from(
        "INSERT INTO object_index \
         (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES ",
    );
    let mut values = Vec::with_capacity(updates.len() * 5);
    for (index, update) in updates.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str("(?, ?, ?, ?, ?, 0)");
        values.extend([
            Value::from(update.hash.clone()),
            Value::from(update.obj_type.clone()),
            Value::from(update.size),
            Value::from(repo_id.to_string()),
            Value::from(created_at),
        ]);
    }
    // A generic blob row may race the semantic agent row for the same OID.
    // Promote to the agent type and mark it unsynced, but never demote an
    // already-semantic row when the generic marker is replayed later.
    sql.push_str(
        " ON CONFLICT(repo_id, o_id) DO UPDATE SET \
         o_type = CASE \
           WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
            AND substr(object_index.o_type, 1, 6) != 'agent_' \
           THEN excluded.o_type ELSE object_index.o_type END, \
         o_size = CASE \
           WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
            AND substr(object_index.o_type, 1, 6) != 'agent_' \
           THEN excluded.o_size ELSE object_index.o_size END, \
         is_synced = CASE \
           WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
            AND substr(object_index.o_type, 1, 6) != 'agent_' \
           THEN 0 ELSE object_index.is_synced END",
    );

    let mut last_error = None;
    for attempt in 1..=INDEX_UPDATE_MAX_ATTEMPTS {
        let statement = Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            sql.clone(),
            values.clone(),
        );
        match db_conn.execute_raw(statement).await {
            Ok(_) if db_path.is_file() => return Ok(()),
            Ok(_) => {
                return Err(format!(
                    "object-index database disappeared during durable repair: {}",
                    db_path.display()
                ));
            }
            Err(error) => {
                last_error = Some(error);
                if attempt < INDEX_UPDATE_MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
            }
        }
    }
    Err(format!(
        "failed to reconcile {} durable object-index repair marker(s) in {}: {}",
        updates.len(),
        db_path.display(),
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "database update failed".to_string())
    ))
}

/// Resolve repository ID for object-index rows.
///
/// Boundary conditions:
/// - Returns the literal string `"unknown-repo"` only when the entry is missing or
///   blank. This sentinel is also recognised by `get_or_create_repo_id_for_prefix`
///   as a placeholder that should be re-rolled on first use.
/// - Propagates query failures. Import turns them into its durable partial/repair
///   contract; ordinary writers retain an atomic repair marker and the top-level
///   CLI warns until a later schema-aware preflight replays it.
async fn resolve_repo_id_for_index(db_conn: &DatabaseConnection) -> Result<String, String> {
    match ConfigKv::get_with_conn(db_conn, "libra.repoid").await {
        Ok(Some(entry)) if !entry.value.trim().is_empty() => Ok(entry.value),
        Ok(_) => Ok("unknown-repo".to_string()),
        Err(err) => Err(format!(
            "Failed to resolve repo id for object index update: {err}"
        )),
    }
}

/// Insert (or no-op) an entry in the `object_index` table with bounded retries on
/// transient failures.
///
/// Functional scope:
/// - Calls [`update_object_index_once`] up to [`INDEX_UPDATE_MAX_ATTEMPTS`] times.
///   SQLite locking errors are normally transient because object writes race with
///   foreground commit/reference updates; dropping the row would make `cloud sync`
///   upload an incomplete object graph.
///
/// Boundary conditions:
/// - A missing database is an error. Every production caller owns a durable
///   repair marker, so treating repository removal or replacement as success
///   would retire the only evidence for a row that was never reconciled.
async fn update_object_index(
    db_path: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) -> Result<(), String> {
    if cfg!(debug_assertions) && std::env::var_os("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL").is_some() {
        return Err("injected object index update failure".to_string());
    }
    let mut last_err = None;

    for attempt in 1..=INDEX_UPDATE_MAX_ATTEMPTS {
        match update_object_index_once(db_path, o_id, o_type, o_size).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt == INDEX_UPDATE_MAX_ATTEMPTS {
                    last_err = Some(err);
                    break;
                }

                tracing::debug!(
                    db_path = %db_path.display(),
                    object_id = o_id,
                    attempt,
                    max_attempts = INDEX_UPDATE_MAX_ATTEMPTS,
                    error = %err,
                    "Retrying object index update after transient failure"
                );
                let delay_ms = 100 * attempt as u64;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "object index update failed".to_string()))
}

/// Update `object_index` for cloud backup tracking — single attempt.
///
/// Functional scope:
/// - Looks up the existing `(o_id, repo_id)` row; inserts only when missing.
/// - Uses a short 200 ms busy timeout so foreground commit/reflog/etc. operations
///   are not blocked by indexing. The outer retry loop gives longer lock windows
///   time to clear without holding contention for a full second at a time.
///
/// Boundary conditions:
/// - Returns `Err` when the database is absent or disappears. The queue keeps
///   its durable repair marker so a later schema-aware command can replay it.
/// - Returns `Err` when the database path is not valid UTF-8 — sea-orm requires a
///   string URL.
/// - See: `update_object_index_rejects_missing_database` and
///   `queued_update_keeps_marker_until_a_moved_database_is_restored`.
async fn update_object_index_once(
    db_path: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) -> Result<(), String> {
    if !db_path.exists() {
        return Err(format!(
            "object-index database is missing while reconciling durable repair: {}",
            db_path.display()
        ));
    }

    let db_path_str = db_path.to_str().ok_or_else(|| {
        format!(
            "database path is not valid UTF-8 for object index update: {}",
            db_path.display()
        )
    })?;

    // Background indexing is best-effort but must not lose rows during ordinary
    // commit-time SQLite lock windows; the outer retry loop handles longer locks.
    let db_conn =
        match db::establish_connection_with_busy_timeout(db_path_str, Duration::from_millis(200))
            .await
        {
            Ok(conn) => conn,
            Err(err) => {
                return Err(format!(
                    "Failed to connect to object index database {}: {}",
                    db_path.display(),
                    err
                ));
            }
        };

    let repo_id = resolve_repo_id_for_index(&db_conn).await?;
    let created_at = chrono::Utc::now().timestamp();

    // Check if object already exists
    // With multi-repo support, we must check (o_id, repo_id)
    use sea_orm::{ActiveModelTrait, Set};
    let existing = object_index::Entity::find()
        .filter(object_index::Column::OId.eq(o_id))
        .filter(object_index::Column::RepoId.eq(&repo_id))
        .one(&db_conn)
        .await;

    let existing = match existing {
        Ok(existing) => existing,
        Err(err) => return Err(format!("Database query failed: {}", err)),
    };

    if let Some(existing_row) = existing {
        // Phase 3.5c codex review: a row may already exist with the
        // generic `blob` tag (written by the standard storage path)
        // before the agent capture runtime calls back with a more
        // specific `agent_transcript` tag for the same content-addressed
        // OID. Without this upgrade the `agent_transcript` tag would be
        // silently dropped — first-writer-wins — and downstream tooling
        // that filters by o_type would never see the captured
        // transcripts. We promote a generic tag to the agent-specific
        // one but never demote in the other direction (a row already
        // tagged `agent_transcript` is left alone).
        if existing_row.o_type != o_type
            && o_type.starts_with("agent_")
            && !existing_row.o_type.starts_with("agent_")
        {
            let mut active: object_index::ActiveModel = existing_row.into();
            active.o_type = Set(o_type.to_string());
            active.is_synced = Set(0);
            if let Err(err) = active.update(&db_conn).await {
                return Err(format!("Failed to upgrade object_index o_type: {}", err));
            }
        }
        if !db_path.is_file() {
            return Err(format!(
                "object-index database disappeared after row reconciliation: {}",
                db_path.display()
            ));
        }
        return Ok(());
    }

    // Insert new object index entry
    let entry = object_index::ActiveModel {
        o_id: Set(o_id.to_string()),
        o_type: Set(o_type.to_string()),
        o_size: Set(o_size),
        repo_id: Set(repo_id),
        created_at: Set(created_at),
        is_synced: Set(0), // Not synced to cloud yet
        ..Default::default()
    };

    if let Err(err) = entry.insert(&db_conn).await {
        return Err(format!("Failed to insert object index: {}", err));
    }

    if !db_path.is_file() {
        return Err(format!(
            "object-index database disappeared after row reconciliation: {}",
            db_path.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use git_internal::{
        errors::GitError,
        hash::{HashKind, get_hash_kind, set_hash_kind, set_hash_kind_for_test},
        internal::{
            metadata::{EntryMeta, MetaAttached},
            object::{ObjectTrait, blob::Blob},
            pack::{encode::PackEncoder, entry::Entry},
        },
    };
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::{
        ClientStorage, ObjectReadFailure, acquire_index_repair_lock,
        remove_object_index_rows_with_conn, resolve_env_sync, update_object_index,
        update_object_index_once,
    };
    use crate::{
        internal::{
            config::ConfigKv,
            db,
            model::{object_index, reference},
        },
        utils::{
            object_ext::BlobExt,
            test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
        },
    };

    /// Test helper that clears an env var on construction and restores it on drop.
    /// Combined with `#[serial]`, this lets tests assert behaviour when a specific
    /// env var is unset without leaking state into sibling tests.
    struct ClearedEnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl ClearedEnvVarGuard {
        fn new(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: these tests are `#[serial]`, so process env mutation is isolated.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for ClearedEnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the exact previous value for the same process env key.
            unsafe {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    // Helper to build packs (copied from previous version for tests)
    async fn encode_entries_to_pack_bytes(entries: Vec<Entry>) -> Result<Vec<u8>, GitError> {
        assert!(!entries.is_empty(), "encode requires at least one entry");
        let (pack_tx, mut pack_rx) = mpsc::channel::<Vec<u8>>(128);
        let (entry_tx, entry_rx) = mpsc::channel::<MetaAttached<Entry, EntryMeta>>(entries.len());
        let mut encoder = PackEncoder::new(entries.len(), 0, pack_tx);
        let kind = get_hash_kind();
        let encode_handle = tokio::spawn(async move {
            set_hash_kind(kind);
            encoder.encode(entry_rx).await
        });

        for entry in entries {
            entry_tx
                .send(MetaAttached {
                    inner: entry,
                    meta: EntryMeta::new(),
                })
                .await
                .map_err(|e| GitError::PackEncodeError(format!("send entry failed: {e}")))?;
        }
        drop(entry_tx);

        let mut pack_bytes = Vec::new();
        while let Some(chunk) = pack_rx.recv().await {
            pack_bytes.extend_from_slice(&chunk);
        }

        let encode_result = encode_handle
            .await
            .map_err(|e| GitError::PackEncodeError(format!("pack encoder task join error: {e}")))?;
        encode_result?;
        Ok(pack_bytes)
    }

    fn build_pack_bytes(entries: Vec<Entry>) -> Result<Vec<u8>, GitError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(encode_entries_to_pack_bytes(entries))
    }

    fn write_pack_to_objects(
        pack_bytes: &[u8],
        label: &str,
    ) -> Result<(tempfile::TempDir, PathBuf, PathBuf), GitError> {
        let dir = tempdir()?;
        let objects_dir = dir.path().join("objects");
        let pack_dir = objects_dir.join("pack");
        fs::create_dir_all(&pack_dir)?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pack_path = pack_dir.join(format!("client-storage-{label}-{unique}.pack"));
        fs::write(&pack_path, pack_bytes)?;
        Ok((dir, objects_dir, pack_path))
    }

    #[test]
    fn object_index_repair_lock_wait_is_bounded() {
        let storage = tempdir().expect("create storage directory");
        let db_path = storage.path().join("libra.db");
        let oid = "a".repeat(40);
        let _held = acquire_index_repair_lock(&db_path, &oid).expect("acquire first repair lock");

        let started = Instant::now();
        let error = match acquire_index_repair_lock(&db_path, &oid) {
            Ok(_) => panic!("a competing repair lock must time out"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= super::INDEX_REPAIR_LOCK_WAIT_TIMEOUT);
        assert!(
            error
                .to_string()
                .contains("another Libra process may be stalled")
        );
    }

    #[test]
    fn object_index_repair_locks_are_isolated_across_shards() {
        let storage = tempdir().expect("create storage directory");
        let db_path = storage.path().join("libra.db");
        let first_oid = format!("aa{}", "1".repeat(38));
        let second_oid = format!("aa{}", "2".repeat(38));
        let _first = acquire_index_repair_lock(&db_path, &first_oid)
            .expect("acquire first object-index repair shard");
        let _second = acquire_index_repair_lock(&db_path, &second_oid)
            .expect("objects in different shards must use independent repair locks");
    }

    #[test]
    fn object_index_repair_lock_namespace_is_bounded_and_stable() {
        let storage = tempdir().expect("create storage directory");
        let db_path = storage.path().join("libra.db");
        let first_oid = format!("abcd{}", "1".repeat(36));
        let same_shard_oid = format!("abcd{}", "2".repeat(36));
        let other_shard_oid = format!("abce{}", "1".repeat(36));

        let first_path = super::index_repair_lock_path(&db_path, &first_oid)
            .expect("resolve first object-index repair lock path");
        let same_shard_path = super::index_repair_lock_path(&db_path, &same_shard_oid)
            .expect("resolve same-shard object-index repair lock path");
        let other_shard_path = super::index_repair_lock_path(&db_path, &other_shard_oid)
            .expect("resolve other-shard object-index repair lock path");

        assert_eq!(first_path, same_shard_path);
        assert_ne!(first_path, other_shard_path);
        assert_eq!(
            first_path.file_name().and_then(|name| name.to_str()),
            Some("abcd.lock")
        );
    }

    /// Scenario: a freshly-built SHA-1 pack must be readable through `ClientStorage`
    /// without the caller having touched any database. Guards the pack-reading code
    /// path that `clone`/`fetch` rely on so they can fall back to packs when the
    /// loose-object directory is absent.
    #[test]
    #[serial]
    fn client_storage_reads_pack_sha1() -> Result<(), GitError> {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let blob = Blob::from_content("client-storage-sha1");
        let pack_bytes = build_pack_bytes(vec![Entry::from(blob.clone())])?;
        let (_tmp, objects_dir, _) = write_pack_to_objects(&pack_bytes, "sha1")?;

        let storage = ClientStorage::init(objects_dir);
        let data = storage.get(&blob.id)?;
        assert_eq!(data, blob.data);
        Ok(())
    }

    /// Pins `classify_read_failure` to the real storage error paths: the
    /// mapping relies on message details produced by `storage/local.rs`,
    /// `storage/load_cost.rs` and `storage/tiered.rs`, so exercise those
    /// paths end-to-end instead of matching hand-written strings.
    #[test]
    #[serial]
    fn classify_read_failure_pins_storage_messages() -> Result<(), GitError> {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = ClientStorage::init_local(tmp.path().join("objects"));

        // Missing: never-written object.
        let ghost = Blob::from_content("classify-missing-object");
        let missing = storage.get(&ghost.id).expect_err("object must be absent");
        assert_eq!(
            ClientStorage::classify_read_failure(&missing),
            ObjectReadFailure::Missing
        );

        // TooLarge: bounded read below the object's load cost.
        let big = Blob::from_content(&"x".repeat(4096));
        storage.put(
            &big.id,
            &big.data,
            git_internal::internal::object::types::ObjectType::Blob,
        )?;
        let too_large = storage
            .get_with_limit(&big.id, 8)
            .expect_err("limit below cost must refuse");
        assert_eq!(
            ClientStorage::classify_read_failure(&too_large),
            ObjectReadFailure::TooLarge
        );

        // Corrupt: truncate the loose file so the zlib/header decode fails.
        let corrupt_victim = Blob::from_content("classify-corrupt-object");
        storage.put(
            &corrupt_victim.id,
            &corrupt_victim.data,
            git_internal::internal::object::types::ObjectType::Blob,
        )?;
        let hex = corrupt_victim.id.to_string();
        let loose = tmp.path().join("objects").join(&hex[..2]).join(&hex[2..]);
        assert!(
            loose.is_file(),
            "loose object expected at {}",
            loose.display()
        );
        // Loose objects are written read-only; lift the mode before corrupting.
        let mut perms = std::fs::metadata(&loose)
            .expect("loose metadata")
            .permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&loose, perms).expect("make loose writable");
        std::fs::write(&loose, b"not a zlib stream").expect("corrupt loose object");
        let corrupt = storage
            .get(&corrupt_victim.id)
            .expect_err("corrupt loose object must fail");
        assert_eq!(
            ClientStorage::classify_read_failure(&corrupt),
            ObjectReadFailure::Corrupt
        );

        Ok(())
    }

    /// Scenario: parallel test for SHA-256 pack reading. SHA-256 has a different
    /// header layout and crc table; this test pins backwards/forwards compatibility
    /// for repositories created with `core.objectformat=sha256`.
    #[test]
    #[serial]
    fn client_storage_reads_pack_sha256() -> Result<(), GitError> {
        let _guard = set_hash_kind_for_test(HashKind::Sha256);
        let blob = Blob::from_content("client-storage-sha256");
        let pack_bytes = build_pack_bytes(vec![Entry::from(blob.clone())])?;
        let (_tmp, objects_dir, _) = write_pack_to_objects(&pack_bytes, "sha256")?;

        let storage = ClientStorage::init(objects_dir);
        let data = storage.get(&blob.id)?;
        assert_eq!(data, blob.data);
        Ok(())
    }

    /// Scenario: round-trip a blob through `put`/`exist`/`get`. This is the smallest
    /// possible regression check that the synchronous facade and its blocking-on-
    /// runtime bridge are wired up correctly.
    #[test]
    #[serial]
    fn test_content_store() {
        let content = "Hello, world!";
        let blob = Blob::from_content(content);

        let _tmp = tempdir().unwrap();
        let source = _tmp.path().join("objects");

        let client_storage = ClientStorage::init(source.clone());
        assert!(
            client_storage
                .put(&blob.id, &blob.data, blob.get_type())
                .is_ok()
        );
        assert!(client_storage.exist(&blob.id));

        let data = client_storage.get(&blob.id).unwrap();
        assert_eq!(data, blob.data);
        assert_eq!(String::from_utf8(data).unwrap(), content);
    }

    /// Scenario: searching for a freshly-stored object by its full hash must return a
    /// non-empty result. This guards the storage-search wiring that downstream
    /// commands (`cat-file`, `rev-parse`) rely on for hash resolution.
    #[tokio::test]
    async fn test_search() {
        let blob = Blob::from_content("Hello, world!");

        let _tmp = tempdir().unwrap();
        let source = _tmp.path().join("objects");

        let client_storage = ClientStorage::init(source.clone());
        assert!(
            client_storage
                .put(&blob.id, &blob.data, blob.get_type())
                .is_ok()
        );

        // Search by full hash should return it
        let objs = client_storage.search(&blob.id.to_string()).await;
        assert!(!objs.is_empty());
    }

    /// Scenario: when a branch row exists but its `commit` column is not a valid
    /// hash, navigation like `main~1` must surface a fatal error rather than
    /// silently returning an empty match list. This protects users from acting on
    /// stale or corrupt references without realising it.
    #[tokio::test]
    #[serial]
    async fn test_search_result_surfaces_corrupt_branch_storage() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db_conn = db::get_db_conn_instance().await;
        reference::ActiveModel {
            name: Set(Some("main".to_string())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(Some("not-a-valid-hash".to_string())),
            remote: Set(None),
            ..Default::default()
        }
        .insert(&db_conn)
        .await
        .unwrap();

        let storage = ClientStorage::init(crate::utils::path::objects());
        let error = storage
            .search_result("main~1")
            .await
            .expect_err("corrupt branch storage should be surfaced");
        assert!(
            error
                .to_string()
                .contains("stored branch reference 'main' is corrupt"),
            "unexpected error: {error}"
        );
    }

    /// Scenario: input like `~1` or `^2` has no base ref. Without a guard, those
    /// would degenerate into a prefix search of the empty string, returning every
    /// object in the repository. The test verifies that we instead return an empty
    /// vector — the safe behaviour for invalid navigation requests.
    #[tokio::test]
    #[serial]
    async fn test_search_result_rejects_empty_base_ref_navigation() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let storage = ClientStorage::init(crate::utils::path::objects());
        assert!(
            storage
                .search_result("~1")
                .await
                .expect("empty-base ~ navigation should not error")
                .is_empty()
        );
        assert!(
            storage
                .search_result("^2")
                .await
                .expect("empty-base ^ navigation should not error")
                .is_empty()
        );
    }

    /// Scenario: zlib compress then decompress must yield the original bytes
    /// verbatim. Pins the compression helpers used by the loose-object writer so a
    /// crate upgrade cannot silently change the round-trip.
    #[test]
    fn test_decompress() {
        let data = b"blob 13\0Hello, world!";
        let compressed_data = ClientStorage::compress_zlib(data).unwrap();
        let decompressed_data = ClientStorage::decompress_zlib(&compressed_data).unwrap();
        assert_eq!(decompressed_data, data);
    }

    /// Scenario: `put` should write its index update to the database that owns the
    /// objects directory it just wrote into, *not* whichever database is reachable
    /// from the process CWD. Regression guard for a bug where two repositories sharing
    /// a CWD could cross-pollinate their object indexes.
    #[tokio::test]
    #[serial]
    async fn background_index_update_uses_storage_database_instead_of_cwd() {
        let workspace = tempdir().unwrap();
        let storage_path = workspace.path().join(".libra");
        fs::create_dir_all(&storage_path).unwrap();
        let objects_dir = storage_path.join("objects");
        fs::create_dir_all(&objects_dir).unwrap();

        let db_path = storage_path.join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(db_path.to_str().unwrap())
            .await
            .unwrap();
        let _ = ConfigKv::set_with_conn(&db_conn, "libra.repoid", "repo-from-storage", false).await;

        // CWD must be the workspace so `try_get_storage_path` can find `.libra/`.
        let _guard = ChangeDirGuard::new(workspace.path());

        let blob = Blob::from_content("index from explicit storage db");
        let storage = ClientStorage::init(objects_dir);
        storage.put(&blob.id, &blob.data, blob.get_type()).unwrap();
        ClientStorage::wait_for_background_tasks();

        let row = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(blob.id.to_string()))
            .filter(object_index::Column::RepoId.eq("repo-from-storage"))
            .one(&db_conn)
            .await
            .unwrap();
        assert!(row.is_some());
    }

    #[tokio::test]
    #[serial]
    async fn durable_index_marker_survives_failure_and_repairs_idempotently() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "repair-repo", false)
            .await
            .expect("set repo id");
        let payload = b"durable repair payload";
        let oid = crate::utils::object::write_git_object(storage.path(), "blob", payload)
            .expect("write repair fixture object");
        let oid = oid.to_string();

        let msg = super::IndexUpdateMsg {
            hash: oid.clone(),
            obj_type: "blob".to_string(),
            size: payload.len() as i64,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        };
        let marker_path = super::persist_index_repair_marker(&msg)
            .expect("persist repair marker before queueing");
        let agent_marker_path = super::persist_index_repair_marker(&super::IndexUpdateMsg {
            hash: oid.clone(),
            obj_type: "agent_transcript".to_string(),
            size: payload.len() as i64,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        })
        .expect("persist distinct semantic-type repair marker");
        assert_ne!(marker_path, agent_marker_path);
        let loose_path = storage
            .path()
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        fs::remove_file(&loose_path)
            .expect("evict local payload after its successful write and durable marker");

        {
            let _failure = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
            let error = ClientStorage::repair_pending_object_index_updates(&db_path)
                .await
                .expect_err("injected index failure should preserve the marker");
            assert!(error.contains("injected object index update failure"));
            assert!(marker_path.is_file());
            assert!(agent_marker_path.is_file());
        }

        assert_eq!(
            ClientStorage::repair_pending_object_index_updates(&db_path)
                .await
                .expect("repair pending row")
                .repaired,
            2
        );
        assert!(!marker_path.exists());
        assert!(!agent_marker_path.exists());
        let row = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(&oid))
            .filter(object_index::Column::RepoId.eq("repair-repo"))
            .one(&db_conn)
            .await
            .expect("query repaired row");
        assert!(row.is_some());
        assert_eq!(
            row.expect("repaired row should exist").o_type,
            "agent_transcript",
            "generic replay must not demote the agent-specific semantic type"
        );
        assert_eq!(
            ClientStorage::repair_pending_object_index_updates(&db_path)
                .await
                .expect("second repair should be a no-op")
                .repaired,
            0
        );
    }

    #[tokio::test]
    #[serial]
    async fn blob_save_returns_marker_error_and_retry_recreates_the_marker() {
        ClientStorage::wait_for_background_tasks();
        let repo = tempdir().expect("create temporary repository");
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let marker_dir = repo.path().join(".libra/object-index-repair");
        fs::write(&marker_dir, b"conflicting non-directory")
            .expect("inject marker-directory creation failure");
        let blob = Blob::from_content("legacy save repair retry");

        let first = blob.save();
        assert!(
            first.is_err(),
            "the fallible public API must return marker registration failures"
        );
        let oid = blob.id.to_string();
        assert!(
            crate::utils::path::objects()
                .join(&oid[..2])
                .join(&oid[2..])
                .is_file(),
            "the injected failure should happen after the payload write"
        );

        fs::remove_file(&marker_dir).expect("remove marker-directory conflict");
        assert_eq!(
            blob.save().expect("retry fallible blob save"),
            blob.id,
            "retrying the public API should register the existing payload"
        );
        ClientStorage::wait_for_background_tasks();

        let db_conn = db::get_db_conn_instance().await;
        assert_eq!(
            object_index::Entity::find()
                .filter(object_index::Column::OId.eq(oid))
                .count(&db_conn)
                .await
                .expect("count object-index rows after legacy save retry"),
            1
        );
    }

    #[tokio::test]
    #[serial]
    async fn repair_queue_replays_bounded_pages_without_permanent_cap_failure() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "paged-repair-repo", false)
            .await
            .expect("set repo id");

        for index in 1..=super::INDEX_REPAIR_MARKER_PAGE_CAP + 1 {
            super::persist_index_repair_marker(&super::IndexUpdateMsg {
                hash: format!("{index:040x}"),
                obj_type: "blob".to_string(),
                size: index as i64,
                db_path: db_path.clone(),
                marker_path: None,
                _marker_lock: None,
                failure_counter: super::current_index_failure_counter(),
                pending_counter: super::current_index_pending_counter(),
            })
            .expect("persist paged repair marker");
        }

        let first = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("repair first bounded page");
        assert_eq!(first.repaired, super::INDEX_REPAIR_BATCH_SIZE);
        assert!(
            first.remaining,
            "one batch should remain for the next replay"
        );

        let second = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("repair final bounded page");
        assert_eq!(
            second.repaired,
            super::INDEX_REPAIR_MARKER_PAGE_CAP + 1 - super::INDEX_REPAIR_BATCH_SIZE
        );
        assert!(!second.remaining);
        assert_eq!(
            object_index::Entity::find()
                .filter(object_index::Column::RepoId.eq("paged-repair-repo"))
                .count(&db_conn)
                .await
                .expect("count repaired rows"),
            (super::INDEX_REPAIR_MARKER_PAGE_CAP + 1) as u64
        );
    }

    #[tokio::test]
    #[serial]
    async fn legacy_scratch_cannot_starve_a_real_repair_marker() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "scratch-repair-repo", false)
            .await
            .expect("set repo id");
        let marker_dir = storage.path().join(super::INDEX_REPAIR_MARKER_DIR);
        fs::create_dir_all(&marker_dir).expect("create marker directory");
        for index in 0..=super::INDEX_REPAIR_MARKER_PAGE_CAP {
            fs::write(marker_dir.join(format!(".tmpLegacy{index:04}")), b"partial")
                .expect("write legacy scratch fixture");
        }
        super::persist_index_repair_marker(&super::IndexUpdateMsg {
            hash: "0123456789abcdef0123456789abcdef01234567".to_string(),
            obj_type: "blob".to_string(),
            size: 42,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        })
        .expect("persist real repair marker behind scratch fixtures");

        let mut repaired = 0;
        let mut remaining = true;
        for _ in 0..=super::INDEX_REPAIR_MARKER_PAGE_CAP + 2 {
            let outcome = ClientStorage::repair_pending_object_index_updates(&db_path)
                .await
                .expect("bounded repair invocation should make progress");
            repaired += outcome.repaired;
            remaining = outcome.remaining;
            if !remaining {
                break;
            }
        }
        assert_eq!(repaired, 1, "the real marker must eventually be replayed");
        assert!(
            !remaining,
            "all scratch and marker entries must be consumed"
        );
        assert_eq!(
            fs::read_dir(&marker_dir)
                .expect("read repaired marker directory")
                .count(),
            0,
            "legacy scratch remnants must be scavenged"
        );
    }

    #[tokio::test]
    #[serial]
    async fn malformed_final_repair_filename_fails_closed_but_legacy_scratch_is_scavenged() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        let marker_dir = storage.path().join(super::INDEX_REPAIR_MARKER_DIR);
        fs::create_dir_all(&marker_dir).expect("create marker directory");
        fs::write(marker_dir.join(".tmpAbCd12"), b"partial")
            .expect("write abandoned atomic scratch file");

        let empty = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("known legacy atomic scratch file should be scavenged");
        assert_eq!(empty.repaired, 0);
        assert!(!empty.remaining);
        assert!(
            !marker_dir.join(".tmpAbCd12").exists(),
            "legacy scratch must be removed so it cannot consume every future page"
        );

        fs::write(marker_dir.join("broken.json"), b"{}").expect("write malformed final marker");
        let error = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect_err("malformed final marker filename must fail closed");
        assert!(
            error.contains("invalid object-index repair marker filename"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn repair_rejects_marker_oid_from_the_wrong_repository_hash_format() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        super::persist_index_repair_marker(&super::IndexUpdateMsg {
            hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            obj_type: "blob".to_string(),
            size: 42,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        })
        .expect("persist structurally valid SHA-256 marker");

        let error = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect_err("a SHA-1 repository must reject a SHA-256 marker");
        assert!(
            error.contains("invalid object-index repair marker filename"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn repair_accepts_marker_oid_matching_sha256_repository_format() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "core.objectformat", "sha256", false)
            .await
            .expect("set SHA-256 object format");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "sha256-repair-repo", false)
            .await
            .expect("set repo id");
        super::persist_index_repair_marker(&super::IndexUpdateMsg {
            hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            obj_type: "blob".to_string(),
            size: 42,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        })
        .expect("persist SHA-256 repair marker");

        let outcome = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("matching SHA-256 marker should repair");
        assert_eq!(outcome.repaired, 1);
        assert!(!outcome.remaining);
    }

    #[tokio::test]
    #[serial]
    async fn repair_scavenges_bounded_stale_staging_files() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        let staging_dir = storage.path().join(super::INDEX_REPAIR_MARKER_STAGING_DIR);
        fs::create_dir_all(&staging_dir).expect("create marker staging directory");
        let stale = staging_dir.join(".tmpAbandoned");
        fs::write(&stale, b"partial marker").expect("write abandoned staging file");
        let old = SystemTime::now()
            .checked_sub(super::INDEX_REPAIR_STAGING_STALE_AFTER + Duration::from_secs(1))
            .expect("construct stale timestamp");
        fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .expect("open stale staging file")
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("age staging file");

        let outcome = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("repair should scavenge stale staging state");
        assert_eq!(outcome.repaired, 0);
        assert!(!outcome.remaining);
        assert!(!stale.exists(), "stale staging file was not scavenged");
    }

    #[tokio::test]
    #[serial]
    async fn agent_indexing_fails_before_enqueue_when_marker_cannot_be_persisted() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        fs::write(
            storage.path().join(super::INDEX_REPAIR_MARKER_DIR),
            b"conflicting non-directory",
        )
        .expect("create conflicting marker path");

        let error = super::enqueue_agent_blob_object_index_update(
            storage.path(),
            "0123456789abcdef0123456789abcdef01234567",
            "agent_transcript",
            42,
        )
        .expect_err("agent indexing must fail before enqueue without a durable marker");
        assert!(
            error
                .to_string()
                .contains("durable cloud object-index repair marker could not be registered"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn marker_retirement_failure_is_counted_and_remains_repairable() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "retire-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);
        let blob = Blob::from_content("retirement warning");
        let failures_before = ClientStorage::background_index_failure_count();
        {
            let _failure = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_MARKER_RETIRE_FAIL", "1");
            client
                .put(&blob.id, &blob.data, blob.get_type())
                .expect("store object and marker");
            ClientStorage::wait_for_background_tasks();
        }
        assert_eq!(
            ClientStorage::background_index_failure_count(),
            failures_before + 1,
            "marker retirement failure did not enter the foreground warning counter"
        );
        assert_eq!(
            ClientStorage::repair_pending_object_index_updates(&db_path)
                .await
                .expect("retry retained marker")
                .repaired,
            1
        );
    }

    #[tokio::test]
    #[serial]
    async fn late_failure_stays_with_the_invocation_that_enqueued_it() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "scope-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);

        let _failure = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
        let _delay = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "100");
        let blob = Blob::from_content("late invocation-scoped failure");
        let first_scope = ClientStorage::with_background_index_failure_scope(async {
            let scope = ClientStorage::begin_background_index_failure_scope();
            client
                .put(&blob.id, &blob.data, blob.get_type())
                .expect("store object and enqueue delayed index update");
            scope
        })
        .await;

        // A later embedded invocation starts before the old queue task reaches
        // its terminal failure. The message must retain the first scope.
        let second_scope = ClientStorage::with_background_index_failure_scope(async {
            let scope = ClientStorage::begin_background_index_failure_scope();
            ClientStorage::wait_for_background_tasks();
            scope
        })
        .await;
        assert_eq!(first_scope.failure_count(), 1);
        assert_eq!(
            second_scope.failure_count(),
            0,
            "late work from the prior invocation leaked into the next warning scope"
        );
    }

    #[tokio::test]
    #[serial]
    async fn concurrent_direct_storage_work_is_not_charged_to_cli_scope() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "direct-scope-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);

        let _failure = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
        let _delay = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "100");
        let scope = ClientStorage::with_background_index_failure_scope(async {
            let scope = ClientStorage::begin_background_index_failure_scope();
            let direct_client = client.clone();
            tokio::spawn(async move {
                let blob = Blob::from_content("concurrent unscoped storage failure");
                direct_client
                    .put(&blob.id, &blob.data, blob.get_type())
                    .expect("store direct object and enqueue delayed index update");
            })
            .await
            .expect("direct storage task should not panic");

            assert!(
                ClientStorage::wait_for_background_tasks_until(
                    Instant::now() + Duration::from_millis(25)
                )
                .await,
                "an unrelated direct write must not enter the CLI pending-work scope"
            );
            scope
        })
        .await;

        ClientStorage::wait_for_background_tasks();
        assert_eq!(
            scope.failure_count(),
            0,
            "an unrelated direct write must not enter the CLI failure scope"
        );
    }

    #[tokio::test]
    #[serial]
    async fn direct_fifo_backlog_does_not_delay_invocation_scoped_updates() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "lane-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);

        let _delay = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "100");
        for index in 0..8 {
            let blob = Blob::from_content(&format!("unscoped backlog {index}"));
            client
                .put(&blob.id, &blob.data, blob.get_type())
                .expect("enqueue unscoped backlog object");
        }

        ClientStorage::with_background_index_failure_scope(async {
            let blob = Blob::from_content("invocation-scoped lane object");
            client
                .put(&blob.id, &blob.data, blob.get_type())
                .expect("enqueue invocation-scoped object");
            assert!(
                ClientStorage::wait_for_background_tasks_until(
                    Instant::now() + Duration::from_millis(400)
                )
                .await,
                "the invocation lane must drain independently of the earlier direct FIFO backlog"
            );
        })
        .await;

        ClientStorage::wait_for_background_tasks();
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn command_owned_spawn_is_registered_before_it_enqueues_index_work() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "spawn-scope-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);

        let _failure = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1");
        let scope = ClientStorage::with_background_index_failure_scope(async move {
            let scope = ClientStorage::begin_background_index_failure_scope();
            let producer = ClientStorage::spawn_background_index_work(async move {
                tokio::task::yield_now().await;
                let blob = Blob::from_content("command-owned spawned storage failure");
                client
                    .put(&blob.id, &blob.data, blob.get_type())
                    .expect("store spawned object and enqueue index update");
            });

            assert!(
                ClientStorage::wait_for_background_tasks_until(
                    Instant::now() + Duration::from_secs(2)
                )
                .await,
                "the invocation drain must wait for the producer and its queued update"
            );
            producer.await.expect("spawned producer should not panic");
            scope
        })
        .await;

        assert_eq!(
            scope.failure_count(),
            1,
            "the spawned producer's terminal failure must stay with its invocation"
        );
    }

    #[tokio::test]
    #[serial]
    async fn replay_retirement_fences_a_delayed_queued_writer_after_prune() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should be UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "ownership-repo", false)
            .await
            .expect("set repo id");
        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);
        let blob = Blob::from_content("delayed writer must not resurrect a pruned row");

        let _delay = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "500");
        client
            .put(&blob.id, &blob.data, blob.get_type())
            .expect("store object and enqueue delayed index update");

        let replay = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("foreground replay owns and retires the marker");
        assert_eq!(replay.repaired, 1);
        assert!(!replay.remaining);

        object_index::Entity::delete_many()
            .filter(object_index::Column::RepoId.eq("ownership-repo"))
            .filter(object_index::Column::OId.eq(blob.id.to_string()))
            .exec(&db_conn)
            .await
            .expect("simulate destructive prune after replay");
        ClientStorage::wait_for_background_tasks();

        assert_eq!(
            object_index::Entity::find()
                .filter(object_index::Column::RepoId.eq("ownership-repo"))
                .filter(object_index::Column::OId.eq(blob.id.to_string()))
                .count(&db_conn)
                .await
                .expect("count rows after delayed writer drains"),
            0,
            "a queued writer whose marker ownership was retired must not resurrect the row"
        );
    }

    #[tokio::test]
    #[serial]
    async fn deletion_fence_refuses_an_oid_with_a_durable_marker() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let oid = "0123456789abcdef0123456789abcdef01234567".to_string();
        super::persist_index_repair_marker(&super::IndexUpdateMsg {
            hash: oid.clone(),
            obj_type: "blob".to_string(),
            size: 42,
            db_path: db_path.clone(),
            marker_path: None,
            _marker_lock: None,
            failure_counter: super::current_index_failure_counter(),
            pending_counter: super::current_index_pending_counter(),
        })
        .expect("persist deletion-conflict marker");

        let error = super::acquire_object_index_deletion_fence(&db_path, &[oid])
            .await
            .expect_err("destructive deletion must fail closed while a marker exists");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(error.to_string().contains("durable repair marker"));
    }

    #[tokio::test]
    #[serial]
    async fn deletion_fence_blocks_new_marker_publication_until_released() {
        let storage = tempdir().expect("create storage dir");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let oid = "fedcba9876543210fedcba9876543210fedcba98".to_string();
        let fence =
            super::acquire_object_index_deletion_fence(&db_path, std::slice::from_ref(&oid))
                .await
                .expect("acquire deletion fence")
                .expect("non-empty OID set must return a fence");

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let publisher = std::thread::spawn(move || {
            started_tx.send(()).ok();
            let result = super::persist_index_repair_marker(&super::IndexUpdateMsg {
                hash: oid,
                obj_type: "blob".to_string(),
                size: 42,
                db_path,
                marker_path: None,
                _marker_lock: None,
                failure_counter: super::current_index_failure_counter(),
                pending_counter: super::current_index_pending_counter(),
            });
            result_tx.send(result).ok();
        });
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("marker publisher started");
        assert!(
            result_rx.recv_timeout(Duration::from_millis(25)).is_err(),
            "marker publication crossed a held destructive-deletion fence"
        );

        drop(fence);
        result_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("marker publisher resumed after fence release")
            .expect("persist marker after fence release");
        publisher.join().expect("marker publisher thread");
    }

    /// A missing database cannot be treated as successful reconciliation: doing
    /// so would let the queue retire its only durable repair marker.
    #[tokio::test]
    #[serial]
    async fn update_object_index_rejects_missing_database() {
        let missing_root = tempdir().unwrap();
        let missing_db = missing_root.path().join(crate::utils::util::DATABASE);

        let result = update_object_index(&missing_db, "deadbeef", "blob", 12).await;
        let error = result.expect_err("a missing repository database must preserve repair work");
        assert!(error.contains("object-index database is missing"));
    }

    #[tokio::test]
    #[serial]
    async fn queued_update_keeps_marker_until_a_moved_database_is_restored() {
        ClientStorage::wait_for_background_tasks();
        let storage = tempdir().expect("create storage directory");
        let db_path = storage.path().join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(
            db_path
                .to_str()
                .expect("temporary database path should contain valid UTF-8"),
        )
        .await
        .expect("create database");
        ConfigKv::set_with_conn(&db_conn, "libra.repoid", "moved-db-repo", false)
            .await
            .expect("set repository id");
        db_conn.close().await.expect("close setup database");

        let objects = storage.path().join("objects");
        fs::create_dir_all(&objects).expect("create object directory");
        let client = ClientStorage::init_local(objects);
        let blob = Blob::from_content("database path must remain durable");
        let marker_path = super::index_repair_marker_path(&db_path, &blob.id.to_string(), "blob")
            .expect("derive marker path");
        let moved_db_path = storage.path().join("libra.db.moved");

        let _delay = ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "250");
        let scope = ClientStorage::with_background_index_failure_scope(async {
            client
                .put(&blob.id, &blob.data, blob.get_type())
                .expect("store object and durable repair marker");
            assert!(marker_path.is_file(), "marker must precede queue execution");
            fs::rename(&db_path, &moved_db_path).expect("move database out of canonical path");
            ClientStorage::wait_for_background_tasks();
            ClientStorage::begin_background_index_failure_scope()
        })
        .await;

        assert_eq!(scope.failure_count(), 1);
        assert!(
            marker_path.is_file(),
            "a missing canonical database must leave the marker retryable"
        );
        fs::rename(&moved_db_path, &db_path).expect("restore canonical database path");
        let outcome = ClientStorage::repair_pending_object_index_updates(&db_path)
            .await
            .expect("replay marker after restoring database");
        assert_eq!(outcome.repaired, 1);
        assert!(!outcome.remaining);
        assert!(!marker_path.exists());

        let db_conn = db::get_db_conn_instance_for_path(&db_path)
            .await
            .expect("reopen restored database");
        assert_eq!(
            object_index::Entity::find()
                .filter(object_index::Column::RepoId.eq("moved-db-repo"))
                .filter(object_index::Column::OId.eq(blob.id.to_string()))
                .count(&db_conn)
                .await
                .expect("count replayed object-index row"),
            1
        );
    }

    #[tokio::test]
    async fn remove_object_index_rows_fails_when_repo_id_cannot_be_resolved() {
        use sea_orm::{ConnectionTrait, Statement};

        let tmp = tempdir().expect("create temporary database directory");
        let db_path = tmp.path().join(crate::utils::util::DATABASE);
        let conn = db::create_database(
            db_path
                .to_str()
                .expect("test database path must contain valid UTF-8"),
        )
        .await
        .expect("create test database");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TABLE config_kv".to_string(),
        ))
        .await
        .expect("remove repository-id source table");

        let error = remove_object_index_rows_with_conn(
            &conn,
            &["0123456789012345678901234567890123456789".to_string()],
        )
        .await
        .expect_err("repository-id lookup failure must abort object-index cleanup");

        assert!(
            error.to_string().contains("no such table: config_kv"),
            "unexpected cleanup error: {error}"
        );
    }

    /// Phase 3.5c codex round-2 follow-up: a row written first by the
    /// standard storage path with `o_type='blob'` must be UPGRADED to
    /// the agent-specific tag (`agent_transcript`) when the agent
    /// capture call back arrives for the same content-addressed OID.
    /// This is the regression case the round-1 review flagged: a naive
    /// "skip if exists" silently kept the generic tag and downstream
    /// tooling that filtered by o_type lost visibility on captured
    /// transcripts. We exercise the upgrade branch directly here.
    #[tokio::test]
    #[serial]
    async fn update_object_index_upgrades_generic_blob_to_agent_specific_o_type() {
        use sea_orm::{ConnectionTrait, Statement};

        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join(crate::utils::util::DATABASE);
        let conn = db::create_database(db_path.to_str().expect("test database path is UTF-8"))
            .await
            .expect("create database");

        // Seed a generic-blob row that mimics the standard storage path.
        // The repo_id matches the sentinel returned by
        // `resolve_repo_id_for_index` when `libra.repoid` is absent. That
        // keeps the seeded row aligned with what `update_object_index_once`
        // queries while preserving the full current schema contract.
        const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
             VALUES (?, 'blob', 42, 'unknown-repo', 0, 1)",
            [OID.into()],
        ))
        .await
        .unwrap();

        // First call: agent-specific tag arrives. The row must be
        // promoted in place; o_id stays unique.
        update_object_index_once(&db_path, OID, "agent_transcript", 42)
            .await
            .expect("upgrade ok");

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT o_type, is_synced FROM object_index WHERE o_id = ? LIMIT 1",
                [OID.into()],
            ))
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(
            row.try_get_by::<String, _>("o_type").unwrap(),
            "agent_transcript",
            "blob row must upgrade to agent_transcript"
        );
        assert_eq!(
            row.try_get_by::<i64, _>("is_synced").unwrap(),
            0,
            "a promoted row must be offered to cloud sync again"
        );

        // Second call: a *generic* tag arrives for an OID that is
        // already agent-specific. The row must NOT demote — that would
        // strip the spec-mandated tag from the catalogue.
        update_object_index_once(&db_path, OID, "blob", 42)
            .await
            .expect("no-op ok");
        let row_again = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT o_type FROM object_index WHERE o_id = ? LIMIT 1",
                [OID.into()],
            ))
            .await
            .unwrap()
            .expect("row still exists");
        assert_eq!(
            row_again.try_get_by::<String, _>("o_type").unwrap(),
            "agent_transcript",
            "no demotion: agent_transcript stays sticky"
        );

        // Third call: same agent tag, same OID — idempotent no-op.
        update_object_index_once(&db_path, OID, "agent_transcript", 42)
            .await
            .expect("idempotent ok");

        let count_row = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
                [OID.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = count_row.try_get_by("n").unwrap();
        assert_eq!(count, 1, "single row preserved through upgrade + no-op");
    }

    /// Scenario: when the system environment variable is unset, `resolve_env_sync`
    /// must consult the repository's `vault.env.*` config entries. This is the
    /// primary mechanism users rely on to keep storage credentials inside the
    /// repository config rather than in their shell rc.
    #[test]
    #[serial]
    fn resolve_env_sync_reads_non_allowlisted_local_config_values() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _guard = ChangeDirGuard::new(repo.path());
        let _endpoint = ClearedEnvVarGuard::new("LIBRA_STORAGE_ENDPOINT");

        rt.block_on(async {
            ConfigKv::set(
                "vault.env.LIBRA_STORAGE_ENDPOINT",
                "https://storage.example.com",
                false,
            )
            .await
            .unwrap();
        });

        let value = resolve_env_sync("LIBRA_STORAGE_ENDPOINT").unwrap();
        assert_eq!(value.as_deref(), Some("https://storage.example.com"));
    }

    /// Scenario: a corrupt global config file must propagate a fatal error rather
    /// than silently ignoring the global-scope value. Without this guard, an
    /// invalid global config would silently degrade remote storage to local-only
    /// without telling the user anything is wrong.
    #[test]
    #[serial]
    fn resolve_env_sync_surfaces_global_config_connection_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _guard = ChangeDirGuard::new(repo.path());
        let _threshold = ClearedEnvVarGuard::new("LIBRA_STORAGE_THRESHOLD");

        let bad_global_dir = tempdir().unwrap();
        let bad_global_db = bad_global_dir.path().join("bad-global.db");
        fs::write(&bad_global_db, "not sqlite").unwrap();
        let _global_db = ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &bad_global_db);

        let err = resolve_env_sync("LIBRA_STORAGE_THRESHOLD")
            .expect_err("global config connection failure should surface");
        assert!(
            err.contains("failed to connect to global config"),
            "unexpected error: {err}"
        );
    }
}
