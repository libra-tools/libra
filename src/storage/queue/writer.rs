//! Single-writer dual-lane priority-first capture queue consumer (`ADR-RB-05` #4,
//! `RB-09`..`RB-09l`).
//!
//! Guarantees:
//! - Strict priority-first draining: `priority/` (`SessionEnd` and terminal
//!   records) is always drained to its committed boundary before consuming any
//!   batch from `normal/` (and re-checked between `normal/` batches).
//! - Batched single-transaction ingestion: up to `128` frames (or a `50ms`
//!   collection window) are coalesced into a SINGLE
//!   [`CaptureStore::append_capture_batch`] write transaction that atomically
//!   deduplicates on full semantic `dedup_key` and advances the authoritative
//!   DB `QUEUE_HIGHWATER_{PRIORITY,NORMAL}` cursor.
//! - Deadlock-free global lock order: **`writer.lock -> capture-queue/append.lock`**.
//!   Producers NEVER acquire `writer.lock`, and the consumer NEVER holds
//!   `append.lock` across any `CaptureStore` write transaction.
//! - Sealed-segment boundary authentication: every un-reclaimed rotated segment
//!   `min_live_seg <= seg < active_seg` is authenticated against `LTM2.sealed[]`
//!   (`actual_len == sealed_len` and `crc32c(seg[0..sealed_len]) == sealed_crc32c`)
//!   before consuming frames or advancing highwater across the segment boundary.
//! - Crash-safe eager segment reclamation: strictly after DB highwater advances
//!   past `seg`, delegates to `segment::reclaim_sealed_segment_at_root` under
//!   `writer.lock -> append.lock` (commits dual `LTM2` advancing `MIN_LIVE_SEG`
//!   and removing `sealed[seg]` BEFORE `unlink` + `fsync_dir`).

use std::{
    fs::File,
    io::{self, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(test)]
use crate::storage::queue::segment::{
    SegmentCrashPoint, SegmentObserver, SegmentOpEvent,
    reclaim_sealed_segment_at_root_with_observer, recover_lane_at_root_with_observer,
};
#[cfg(not(test))]
use crate::storage::queue::segment::{
    reclaim_sealed_segment_at_root_with_timeout, recover_lane_at_root_with_timeout,
};
use crate::storage::{
    capture_store::{
        CaptureBatchItem, CaptureBatchPayload, CaptureQueueHighwater, CaptureQueueLane,
        CaptureStore,
    },
    error::{StorageBackendError, StorageResult},
    manifest::PRIVATE_SUBDIR,
    queue::{
        CAPTURE_QUEUE_SUBDIR, FrameStreamDecoder, FrameStreamItem, LTM2_MAX_LEN, LaneTailMeta,
        QUEUE_LANE_NORMAL_DIR, QUEUE_LANE_PRIORITY_DIR, QUEUE_TAIL_META_BAK_FILE,
        QUEUE_TAIL_META_FILE, crc32c_append, decode_postcard_payload,
        highwater::{
            QueueHighwaterSnapshot, compute_next_highwater, load_authoritative_highwater,
            read_highwater_mirror, write_highwater_mirror,
        },
        matches_segment_log_glob_bytes,
        segment::{LaneRecoveryReport, SegmentReclaimOutcome, parse_segment_log_file_name},
        segment_log_file_name,
    },
};

/// Single-writer consumer lock filename under `.libra/private/capture-queue/`.
pub const QUEUE_WRITER_LOCK_FILE: &str = "writer.lock";

/// Postcard schema version carried inside capture queue frames decoded into
/// [`CaptureBatchPayload`].
pub const CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION: u16 = 1;

/// Default and hard maximum frames coalesced into a single
/// [`CaptureStore::append_capture_batch`] write transaction (`ADR-RB-05` #4).
pub const DEFAULT_WRITER_MAX_BATCH_SIZE: usize = 128;

/// Default and hard maximum batch collection window (`ADR-RB-05` #4).
pub const DEFAULT_WRITER_MAX_BATCH_WINDOW: Duration = Duration::from_millis(50);

/// Path to `writer.lock` directly under `queue_root`.
#[must_use]
pub fn writer_lock_path(queue_root: &Path) -> PathBuf {
    queue_root.join(QUEUE_WRITER_LOCK_FILE)
}

/// Runtime options controlling capture queue batch consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueWriterOptions {
    /// Maximum frames coalesced per `CaptureStore::append_capture_batch` tx
    /// (clamped at runtime to `1..=DEFAULT_WRITER_MAX_BATCH_SIZE`).
    pub max_batch_size: usize,
    /// Maximum collection window per batch once the first frame is read
    /// (clamped at runtime to `<= DEFAULT_WRITER_MAX_BATCH_WINDOW`).
    pub max_batch_window: Duration,
}

impl Default for QueueWriterOptions {
    fn default() -> Self {
        Self {
            max_batch_size: DEFAULT_WRITER_MAX_BATCH_SIZE,
            max_batch_window: DEFAULT_WRITER_MAX_BATCH_WINDOW,
        }
    }
}

impl QueueWriterOptions {
    #[must_use]
    pub fn effective_max_batch_size(self) -> usize {
        self.max_batch_size.clamp(1, DEFAULT_WRITER_MAX_BATCH_SIZE)
    }

    #[must_use]
    pub fn effective_max_batch_window(self) -> Duration {
        self.max_batch_window.min(DEFAULT_WRITER_MAX_BATCH_WINDOW)
    }
}

/// Summary report returned by a single consumer drain pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDrainReport {
    /// `true` if this call acquired `writer.lock`; `false` if another active
    /// consumer already held `writer.lock` (no-op contention exit).
    pub acquired_writer_lock: bool,
    /// Number of `CaptureStore::append_capture_batch` transactions committed for `priority/`.
    pub priority_batches: usize,
    /// Number of `CaptureStore::append_capture_batch` transactions committed for `normal/`.
    pub normal_batches: usize,
    /// Total frames applied (newly persisted) across both lanes.
    pub applied_frames: usize,
    /// Total duplicate `dedup_key` frames skipped idempotently across both lanes.
    pub deduplicated_frames: usize,
    /// Number of consumed historical segments safely reclaimed across both lanes.
    pub reclaimed_segments: usize,
    /// Number of `writer.lock -> append.lock` Case C/D recovery delegations executed.
    pub recovery_delegations: usize,
    /// Final authoritative dual-lane highwater snapshot (when lock was acquired).
    pub highwater: Option<QueueHighwaterSnapshot>,
}

impl CaptureDrainReport {
    fn lock_contended() -> Self {
        Self {
            acquired_writer_lock: false,
            priority_batches: 0,
            normal_batches: 0,
            applied_frames: 0,
            deduplicated_frames: 0,
            reclaimed_segments: 0,
            recovery_delegations: 0,
            highwater: None,
        }
    }
}

/// Exclusive non-blocking OS lock guard on `.libra/private/capture-queue/writer.lock`.
///
/// Holding this guard designates the process/task as the sole active capture
/// queue consumer. The underlying OS lock handle is reference-counted so any
/// in-flight `spawn_blocking` recovery/reclamation slice retains `writer.lock`
/// until completion even if the outer async drain future is cancelled/dropped.
#[derive(Debug)]
pub struct WriterLockGuard {
    _file: std::sync::Arc<File>,
}

impl WriterLockGuard {
    /// Clone the underlying lock handle to pin `writer.lock` for the lifetime
    /// of an offloaded `spawn_blocking` recovery or reclamation slice (`RB-09c`).
    fn share_lock_token(&self) -> std::sync::Arc<File> {
        std::sync::Arc::clone(&self._file)
    }
}

/// Attempt a non-blocking acquisition of `.libra/private/capture-queue/writer.lock`
/// under `storage_dir`. Returns `Ok(None)` when another consumer holds the lock.
pub fn try_acquire_writer_lock(storage_dir: &Path) -> StorageResult<Option<WriterLockGuard>> {
    let queue_root = storage_dir.join(PRIVATE_SUBDIR).join(CAPTURE_QUEUE_SUBDIR);
    try_acquire_writer_lock_at_root(&queue_root)
}

/// Attempt a non-blocking acquisition of `writer.lock` directly under `queue_root`.
/// Returns `Ok(None)` when another consumer holds the lock.
pub fn try_acquire_writer_lock_at_root(
    queue_root: &Path,
) -> StorageResult<Option<WriterLockGuard>> {
    #[cfg(unix)]
    {
        unix_writer::try_acquire_writer_lock_unix(queue_root)
    }
    #[cfg(not(unix))]
    {
        non_unix_writer::try_acquire_writer_lock_non_unix(queue_root)
    }
}

/// Probe whether an active capture queue consumer currently holds
/// `.libra/private/capture-queue/writer.lock` (`ADR-RB-05` #4, `RB-09b` AC7).
///
/// Reuses the exact same `writer.lock` file without mutating disk when absent.
pub fn is_writer_active(storage_dir: &Path) -> StorageResult<bool> {
    let queue_root = storage_dir.join(PRIVATE_SUBDIR).join(CAPTURE_QUEUE_SUBDIR);
    is_writer_active_at_root(&queue_root)
}

/// Probe whether an active consumer currently holds `writer.lock` directly
/// under `queue_root` without creating `writer.lock` when absent.
pub fn is_writer_active_at_root(queue_root: &Path) -> StorageResult<bool> {
    #[cfg(unix)]
    {
        unix_writer::is_writer_active_unix(queue_root)
    }
    #[cfg(not(unix))]
    {
        non_unix_writer::is_writer_active_non_unix(queue_root)
    }
}

/// Single-writer dual-lane priority-first capture queue consumer bound to a
/// repository's `queue_root`.
#[derive(Debug, Clone)]
pub struct CaptureQueueWriter {
    queue_root: PathBuf,
    options: QueueWriterOptions,
}

impl CaptureQueueWriter {
    /// Create a writer targeting `<storage_dir>/private/capture-queue`.
    #[must_use]
    pub fn in_storage_dir(storage_dir: &Path) -> Self {
        Self::at_root(storage_dir.join(PRIVATE_SUBDIR).join(CAPTURE_QUEUE_SUBDIR))
    }

    /// Create a writer targeting `queue_root` directly.
    #[must_use]
    pub fn at_root(queue_root: impl Into<PathBuf>) -> Self {
        Self {
            queue_root: queue_root.into(),
            options: QueueWriterOptions::default(),
        }
    }

    /// Configure custom batch size and collection window options (clamped at
    /// runtime to the `128` / `50ms` protocol limits).
    #[must_use]
    pub fn with_options(mut self, options: QueueWriterOptions) -> Self {
        self.options = options;
        self
    }

    /// Queue root path served by this writer.
    #[must_use]
    pub fn queue_root(&self) -> &Path {
        &self.queue_root
    }

    /// Execute one complete priority-first drain pass against `store`.
    ///
    /// If another active writer already holds `writer.lock`, returns
    /// `Ok(CaptureDrainReport { acquired_writer_lock: false, .. })` immediately
    /// with zero side effects.
    pub async fn drain_once(&self, store: &dyn CaptureStore) -> StorageResult<CaptureDrainReport> {
        let Some(lock_guard) = try_acquire_writer_lock_at_root(&self.queue_root)? else {
            return Ok(CaptureDrainReport::lock_contended());
        };
        let mut observer = WriterObserver::new();
        let report = drain_locked_at_root(
            &self.queue_root,
            store,
            self.options,
            &lock_guard,
            &mut observer,
        )
        .await?;
        drop(lock_guard);
        Ok(report)
    }

    /// Acquire `writer.lock`, validate both lanes read-only against authoritative
    /// DB `QUEUE_HIGHWATER_*` and `LTM2` (`[0, committed_len)` + `LTM2.sealed[]`),
    /// and if `lane` has an uncommitted tail (`actual_len > committed_len`),
    /// Case D successor (`max_live_seg > active_seg`), or post-reclaim crash
    /// orphan (`seg < min_live_seg`), delegate shared Case C/D recovery strictly
    /// under `writer.lock -> capture-queue/append.lock` (`ADR-RB-05` #4 ②, `RB-09e`).
    ///
    /// Guarantees:
    /// - NEVER calls [`CaptureStore::append_capture_batch`] (zero DB writes under
    ///   `append.lock`).
    /// - NEVER modifies DB `QUEUE_HIGHWATER_*` or the external `highwater` mirror.
    /// - Returns `Ok(None)` when another consumer holds `writer.lock` or when
    ///   `lane` has no uncommitted tail / successor / crash orphan to recover.
    pub async fn recover_lane_uncommitted_tail(
        &self,
        lane: CaptureQueueLane,
        store: &dyn CaptureStore,
    ) -> StorageResult<Option<LaneRecoveryReport>> {
        let Some(lock_guard) = try_acquire_writer_lock_at_root(&self.queue_root)? else {
            return Ok(None);
        };
        let mut observer = WriterObserver::new();
        let report = recover_lane_uncommitted_tail_locked(
            &self.queue_root,
            lane,
            store,
            &lock_guard,
            &mut observer,
        )
        .await?;
        drop(lock_guard);
        Ok(report)
    }
}

async fn recover_lane_uncommitted_tail_locked(
    queue_root: &Path,
    lane: CaptureQueueLane,
    store: &dyn CaptureStore,
    writer_lock: &WriterLockGuard,
    observer: &mut WriterObserver,
) -> StorageResult<Option<LaneRecoveryReport>> {
    let hw_snapshot = load_authoritative_highwater(store).await?;
    preflight_validate_lane(queue_root, CaptureQueueLane::Priority, hw_snapshot.priority)?;
    preflight_validate_lane(queue_root, CaptureQueueLane::Normal, hw_snapshot.normal)?;

    let ConsumerLaneSnapshot::Populated { meta, seg_ids } =
        inspect_lane_readonly(queue_root, lane)?
    else {
        return Ok(None);
    };
    let has_reclaimed_orphan = seg_ids.iter().any(|&seg| seg < meta.min_live_seg);
    let max_live_seg = seg_ids
        .iter()
        .copied()
        .filter(|&seg| seg >= meta.min_live_seg)
        .max()
        .ok_or_else(|| StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue lane has no live segment files >= min_live_seg".to_string(),
        })?;
    let (_active_file, actual_len) = open_lane_segment_readonly(queue_root, lane, meta.active_seg)?;
    if actual_len > meta.committed_len || max_live_seg > meta.active_seg || has_reclaimed_orphan {
        let rec = call_recover_lane(queue_root, lane, writer_lock, observer).await?;
        return Ok(Some(rec));
    }
    Ok(None)
}

/// Convenience entry point: acquire `writer.lock` and drain `<storage_dir>/private/capture-queue`
/// priority-first into `store` using default `128` / `50ms` batching.
pub async fn drain_capture_queue(
    storage_dir: &Path,
    store: &dyn CaptureStore,
) -> StorageResult<CaptureDrainReport> {
    CaptureQueueWriter::in_storage_dir(storage_dir)
        .drain_once(store)
        .await
}

/// Convenience entry point: acquire `writer.lock` and drain `queue_root`
/// priority-first into `store` using custom options.
pub async fn drain_capture_queue_at_root(
    queue_root: &Path,
    store: &dyn CaptureStore,
    options: QueueWriterOptions,
) -> StorageResult<CaptureDrainReport> {
    CaptureQueueWriter::at_root(queue_root)
        .with_options(options)
        .drain_once(store)
        .await
}

// ============================================================================
// Internal Consumer State Inspection, Verification, and Drain Loop
// ============================================================================

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum WriterCrashPoint {
    #[default]
    None,
    BeforeEmptyAdvanceCommit,
    AfterDbCommitBeforeMirror,
}

#[cfg(test)]
type BeforeAppendLockRecoveryHook = Box<dyn FnMut(CaptureQueueLane) + Send>;

#[cfg(test)]
type AfterAppendLockRecoveryHook = Box<dyn FnMut(&LaneRecoveryReport) + Send>;

#[cfg(test)]
type BeforeAppendLockReclaimHook = Box<dyn FnMut(CaptureQueueLane, u32) + Send>;

#[cfg(test)]
type QueueBusyRetryHook = Box<dyn FnMut(usize) + Send>;

#[cfg(test)]
type BlockingSliceStartHook = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Per-attempt `append.lock` blocking slice offloaded to Tokio's blocking pool
/// when the consumer coordinates recovery or segment reclamation under
/// `writer.lock -> capture-queue/append.lock` (`RB-09c`). Keeping each slice
/// short guarantees async runtimes (including `current_thread`) remain
/// responsive to timers, sibling tasks, and cancellation while waiting on a
/// long-paused producer.
const CONSUMER_APPEND_LOCK_POLL_SLICE: Duration = Duration::from_millis(25);

/// Non-blocking async backoff between `QueueBusy` retry slices.
const CONSUMER_APPEND_LOCK_RETRY_BACKOFF: Duration = Duration::from_millis(5);

#[cfg(test)]
struct WriterObserver {
    crash_point: WriterCrashPoint,
    reclaim_crash_point: SegmentCrashPoint,
    append_lock_slice: Option<Duration>,
    queue_busy_retries: usize,
    consumer_holds_append_lock: std::sync::Arc<std::sync::atomic::AtomicBool>,
    segment_events: Vec<SegmentOpEvent>,
    on_window_poll: Option<Box<dyn FnMut(usize) + Send>>,
    on_sealed_pre_commit: Option<Box<dyn FnMut(u32) + Send>>,
    simulated_elapsed_ms: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    on_before_append_lock_recovery: Option<BeforeAppendLockRecoveryHook>,
    on_after_append_lock_recovery: Option<AfterAppendLockRecoveryHook>,
    on_before_append_lock_reclaim: Option<BeforeAppendLockReclaimHook>,
    on_queue_busy_retry: Option<QueueBusyRetryHook>,
    on_recovery_blocking_slice_started: Option<BlockingSliceStartHook>,
}

#[cfg(test)]
impl Default for WriterObserver {
    fn default() -> Self {
        Self {
            crash_point: WriterCrashPoint::None,
            reclaim_crash_point: SegmentCrashPoint::None,
            append_lock_slice: None,
            queue_busy_retries: 0,
            consumer_holds_append_lock: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            segment_events: Vec::new(),
            on_window_poll: None,
            on_sealed_pre_commit: None,
            simulated_elapsed_ms: None,
            on_before_append_lock_recovery: None,
            on_after_append_lock_recovery: None,
            on_before_append_lock_reclaim: None,
            on_queue_busy_retry: None,
            on_recovery_blocking_slice_started: None,
        }
    }
}

#[cfg(test)]
#[cfg_attr(not(feature = "storage-redb"), allow(dead_code))]
impl WriterObserver {
    fn new() -> Self {
        Self::default()
    }

    fn with_append_lock_slice(mut self, slice: Duration) -> Self {
        self.append_lock_slice = Some(slice);
        self
    }

    fn with_queue_busy_retry_hook(mut self, hook: impl FnMut(usize) + Send + 'static) -> Self {
        self.on_queue_busy_retry = Some(Box::new(hook));
        self
    }

    fn with_recovery_blocking_slice_started_hook(
        mut self,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.on_recovery_blocking_slice_started = Some(std::sync::Arc::new(hook));
        self
    }

    fn notify_queue_busy_retry(&mut self, retry_count: usize) {
        if let Some(hook) = self.on_queue_busy_retry.as_mut() {
            hook(retry_count);
        }
    }

    fn consumer_holds_append_lock_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.consumer_holds_append_lock)
    }

    fn with_crash_point(crash_point: WriterCrashPoint) -> Self {
        Self {
            crash_point,
            ..Self::default()
        }
    }

    fn with_reclaim_crash_point(reclaim_crash_point: SegmentCrashPoint) -> Self {
        Self {
            reclaim_crash_point,
            ..Self::default()
        }
    }

    fn with_window_poll_hook_and_clock(
        hook: impl FnMut(usize) + Send + 'static,
        simulated_elapsed_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            on_window_poll: Some(Box::new(hook)),
            simulated_elapsed_ms: Some(simulated_elapsed_ms),
            ..Self::default()
        }
    }

    fn with_sealed_pre_commit_hook(hook: impl FnMut(u32) + Send + 'static) -> Self {
        Self {
            on_sealed_pre_commit: Some(Box::new(hook)),
            ..Self::default()
        }
    }

    fn with_recovery_hooks(
        before: impl FnMut(CaptureQueueLane) + Send + 'static,
        after: impl FnMut(&LaneRecoveryReport) + Send + 'static,
    ) -> Self {
        Self {
            on_before_append_lock_recovery: Some(Box::new(before)),
            on_after_append_lock_recovery: Some(Box::new(after)),
            ..Self::default()
        }
    }

    fn with_before_reclaim_hook(
        before: impl FnMut(CaptureQueueLane, u32) + Send + 'static,
    ) -> Self {
        Self {
            on_before_append_lock_reclaim: Some(Box::new(before)),
            ..Self::default()
        }
    }

    fn maybe_crash(&self, point: WriterCrashPoint) -> StorageResult<()> {
        if self.crash_point == point {
            return Err(StorageBackendError::io(
                "injected consumer crash point",
                io::Error::other("injected consumer crash point"),
            ));
        }
        Ok(())
    }

    fn notify_window_poll(&mut self, collected_count: usize) {
        if let Some(hook) = self.on_window_poll.as_mut() {
            hook(collected_count);
        }
    }

    fn notify_sealed_pre_commit(&mut self, seg_id: u32) {
        if let Some(hook) = self.on_sealed_pre_commit.as_mut() {
            hook(seg_id);
        }
    }

    fn notify_before_append_lock_recovery(&mut self, lane: CaptureQueueLane) {
        if let Some(hook) = self.on_before_append_lock_recovery.as_mut() {
            hook(lane);
        }
    }

    fn notify_after_append_lock_recovery(&mut self, report: &LaneRecoveryReport) {
        if let Some(hook) = self.on_after_append_lock_recovery.as_mut() {
            hook(report);
        }
    }

    fn notify_before_append_lock_reclaim(&mut self, lane: CaptureQueueLane, seg_id: u32) {
        if let Some(hook) = self.on_before_append_lock_reclaim.as_mut() {
            hook(lane, seg_id);
        }
    }

    fn window_elapsed(&self, start: Instant) -> Duration {
        match &self.simulated_elapsed_ms {
            Some(ms) => Duration::from_millis(ms.load(std::sync::atomic::Ordering::Acquire)),
            None => start.elapsed(),
        }
    }
}

#[cfg(not(test))]
struct WriterObserver;

#[cfg(not(test))]
impl WriterObserver {
    const fn new() -> Self {
        Self
    }

    #[inline]
    fn maybe_crash(&self, _point: ()) -> StorageResult<()> {
        Ok(())
    }

    #[inline]
    fn notify_window_poll(&mut self, _collected_count: usize) {}

    #[inline]
    fn notify_sealed_pre_commit(&mut self, _seg_id: u32) {}

    #[inline]
    fn window_elapsed(&self, start: Instant) -> Duration {
        start.elapsed()
    }
}

#[cfg(test)]
type CrashPointToken = WriterCrashPoint;
#[cfg(not(test))]
type CrashPointToken = ();

#[cfg(test)]
#[inline]
const fn crash_after_commit_before_mirror() -> CrashPointToken {
    WriterCrashPoint::AfterDbCommitBeforeMirror
}

#[cfg(not(test))]
#[inline]
const fn crash_after_commit_before_mirror() -> CrashPointToken {}

#[cfg(test)]
struct ConsumerAppendLockScopeGuard<'a>(&'a std::sync::atomic::AtomicBool);

#[cfg(test)]
impl<'a> ConsumerAppendLockScopeGuard<'a> {
    fn enter(flag: &'a std::sync::atomic::AtomicBool) -> Self {
        flag.store(true, std::sync::atomic::Ordering::Release);
        Self(flag)
    }
}

#[cfg(test)]
impl Drop for ConsumerAppendLockScopeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Delegate sealed-segment reclamation under `writer.lock -> capture-queue/append.lock`,
/// offloading each bounded lock-wait slice onto Tokio's blocking pool so async
/// runtimes (including `current_thread`) never stall while waiting on a paused
/// producer (`RB-09c` AC1–AC4).
async fn call_reclaim_sealed_segment(
    queue_root: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    highwater_seg: u32,
    writer_lock: &WriterLockGuard,
    observer: &mut WriterObserver,
) -> StorageResult<SegmentReclaimOutcome> {
    #[cfg(test)]
    {
        observer.notify_before_append_lock_reclaim(lane, seg_id);
        let slice = observer
            .append_lock_slice
            .unwrap_or(CONSUMER_APPEND_LOCK_POLL_SLICE);
        let crash_point = observer.reclaim_crash_point;
        loop {
            let root = queue_root.to_path_buf();
            let writer_lock_hold = writer_lock.share_lock_token();
            let holds_flag = std::sync::Arc::clone(&observer.consumer_holds_append_lock);
            let (res, events) = tokio::task::spawn_blocking(move || {
                let _writer_lock_hold = writer_lock_hold;
                let mut seg_obs =
                    SegmentObserver::with_crash(crash_point).with_append_lock_timeout(slice);
                let res = {
                    let _guard = ConsumerAppendLockScopeGuard::enter(&holds_flag);
                    reclaim_sealed_segment_at_root_with_observer(
                        &root,
                        lane,
                        seg_id,
                        highwater_seg,
                        &mut seg_obs,
                    )
                };
                (res, seg_obs.events)
            })
            .await
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "capture queue segment reclamation blocking task aborted".to_string(),
            })?;
            observer.segment_events.extend(events);
            match res {
                Err(StorageBackendError::QueueBusy { .. }) => {
                    observer.queue_busy_retries += 1;
                    observer.notify_queue_busy_retry(observer.queue_busy_retries);
                    tokio::time::sleep(CONSUMER_APPEND_LOCK_RETRY_BACKOFF).await;
                    continue;
                }
                other => return other,
            }
        }
    }
    #[cfg(not(test))]
    {
        let _ = observer;
        loop {
            let root = queue_root.to_path_buf();
            let writer_lock_hold = writer_lock.share_lock_token();
            let res = tokio::task::spawn_blocking(move || {
                let _writer_lock_hold = writer_lock_hold;
                reclaim_sealed_segment_at_root_with_timeout(
                    &root,
                    lane,
                    seg_id,
                    highwater_seg,
                    CONSUMER_APPEND_LOCK_POLL_SLICE,
                )
            })
            .await
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "capture queue segment reclamation blocking task aborted".to_string(),
            })?;
            match res {
                Err(StorageBackendError::QueueBusy { .. }) => {
                    tokio::time::sleep(CONSUMER_APPEND_LOCK_RETRY_BACKOFF).await;
                    continue;
                }
                other => return other,
            }
        }
    }
}

/// Delegate lane tail/orphan re-verification and recovery under
/// `writer.lock -> capture-queue/append.lock`, offloading each bounded lock-wait
/// slice onto Tokio's blocking pool so async runtimes (including `current_thread`)
/// remain responsive while waiting on a paused producer (`RB-09c` AC1–AC3).
async fn call_recover_lane(
    queue_root: &Path,
    lane: CaptureQueueLane,
    writer_lock: &WriterLockGuard,
    observer: &mut WriterObserver,
) -> StorageResult<LaneRecoveryReport> {
    #[cfg(test)]
    {
        observer.notify_before_append_lock_recovery(lane);
        let slice = observer
            .append_lock_slice
            .unwrap_or(CONSUMER_APPEND_LOCK_POLL_SLICE);
        let on_started = observer.on_recovery_blocking_slice_started.clone();
        loop {
            let root = queue_root.to_path_buf();
            let writer_lock_hold = writer_lock.share_lock_token();
            let holds_flag = std::sync::Arc::clone(&observer.consumer_holds_append_lock);
            let on_started_clone = on_started.clone();
            let (res, events) = tokio::task::spawn_blocking(move || {
                let _writer_lock_hold = writer_lock_hold;
                if let Some(hook) = on_started_clone {
                    hook();
                }
                let mut seg_obs = SegmentObserver::new().with_append_lock_timeout(slice);
                let res = {
                    let _guard = ConsumerAppendLockScopeGuard::enter(&holds_flag);
                    recover_lane_at_root_with_observer(&root, lane, &mut seg_obs)
                };
                (res, seg_obs.events)
            })
            .await
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "capture queue lane recovery blocking task aborted".to_string(),
            })?;
            observer.segment_events.extend(events);
            match res {
                Err(StorageBackendError::QueueBusy { .. }) => {
                    observer.queue_busy_retries += 1;
                    observer.notify_queue_busy_retry(observer.queue_busy_retries);
                    tokio::time::sleep(CONSUMER_APPEND_LOCK_RETRY_BACKOFF).await;
                    continue;
                }
                Ok(rep) => {
                    observer.notify_after_append_lock_recovery(&rep);
                    return Ok(rep);
                }
                Err(err) => return Err(err),
            }
        }
    }
    #[cfg(not(test))]
    {
        let _ = observer;
        loop {
            let root = queue_root.to_path_buf();
            let writer_lock_hold = writer_lock.share_lock_token();
            let res = tokio::task::spawn_blocking(move || {
                let _writer_lock_hold = writer_lock_hold;
                recover_lane_at_root_with_timeout(&root, lane, CONSUMER_APPEND_LOCK_POLL_SLICE)
            })
            .await
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "capture queue lane recovery blocking task aborted".to_string(),
            })?;
            match res {
                Err(StorageBackendError::QueueBusy { .. }) => {
                    tokio::time::sleep(CONSUMER_APPEND_LOCK_RETRY_BACKOFF).await;
                    continue;
                }
                other => return other,
            }
        }
    }
}

fn lane_subdir_name(lane: CaptureQueueLane) -> &'static str {
    match lane {
        CaptureQueueLane::Priority => QUEUE_LANE_PRIORITY_DIR,
        CaptureQueueLane::Normal => QUEUE_LANE_NORMAL_DIR,
    }
}

#[derive(Debug)]
enum ReplicaRead {
    Absent,
    Corrupt,
    Valid(LaneTailMeta),
}

fn resolve_consumer_replicas(
    primary: ReplicaRead,
    backup: ReplicaRead,
) -> StorageResult<Option<LaneTailMeta>> {
    match (primary, backup) {
        (ReplicaRead::Valid(p), ReplicaRead::Valid(b)) => {
            match p.ordering_key().cmp(&b.ordering_key()) {
                std::cmp::Ordering::Greater => Ok(Some(p)),
                std::cmp::Ordering::Less => Ok(Some(b)),
                std::cmp::Ordering::Equal => {
                    if p != b {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail: "dual LTM2 replicas share ordering key but disagree on checksum or sealed table"
                                .to_string(),
                        });
                    }
                    Ok(Some(p))
                }
            }
        }
        (ReplicaRead::Valid(p), ReplicaRead::Absent | ReplicaRead::Corrupt) => Ok(Some(p)),
        (ReplicaRead::Absent | ReplicaRead::Corrupt, ReplicaRead::Valid(b)) => Ok(Some(b)),
        (ReplicaRead::Absent, ReplicaRead::Absent) => Ok(None),
        (ReplicaRead::Corrupt, ReplicaRead::Absent | ReplicaRead::Corrupt)
        | (ReplicaRead::Absent, ReplicaRead::Corrupt) => {
            Err(StorageBackendError::CorruptCommittedFrame {
                detail: "both LTM2 tail.meta replicas are corrupt or unreadable".to_string(),
            })
        }
    }
}

#[derive(Debug)]
enum ConsumerLaneSnapshot {
    /// 0 segment files and empty/virgin metadata (`(0, 0, 0, 0, [])` or absent).
    Empty,
    /// Active non-empty lane with authenticated authoritative `LTM2` metadata
    /// and sorted on-disk segment ids.
    Populated {
        meta: LaneTailMeta,
        seg_ids: Vec<u32>,
    },
}

fn inspect_lane_readonly(
    queue_root: &Path,
    lane: CaptureQueueLane,
) -> StorageResult<ConsumerLaneSnapshot> {
    #[cfg(unix)]
    {
        unix_writer::inspect_lane_readonly_unix(queue_root, lane)
    }
    #[cfg(not(unix))]
    {
        non_unix_writer::inspect_lane_readonly_non_unix(queue_root, lane)
    }
}

fn open_lane_segment_readonly(
    queue_root: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
) -> StorageResult<(File, u64)> {
    #[cfg(unix)]
    {
        unix_writer::open_lane_segment_readonly_unix(queue_root, lane, seg_id)
    }
    #[cfg(not(unix))]
    {
        non_unix_writer::open_lane_segment_readonly_non_unix(queue_root, lane, seg_id)
    }
}

fn stream_verify_prefix_crc(
    file: &mut File,
    byte_len: u64,
    expected_crc32c: u32,
    mismatch_detail: &'static str,
) -> StorageResult<()> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| StorageBackendError::io("seek capture queue segment for CRC check", e))?;
    let mut crc = 0u32;
    let mut remaining = byte_len;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let n = file
            .read(&mut buf[..want])
            .map_err(|e| StorageBackendError::io("read capture queue segment for CRC check", e))?;
        if n == 0 {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment hit unexpected EOF during CRC verification"
                    .to_string(),
            });
        }
        crc = crc32c_append(crc, &buf[..n]);
        remaining -= n as u64;
    }
    if crc != expected_crc32c {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: mismatch_detail.to_string(),
        });
    }
    Ok(())
}

/// Structurally authenticate every frame boundary, dual CRC32C, and versioned
/// postcard envelope strictly inside `[start_offset, end_offset)`, optionally
/// asserting that `required_boundary` (e.g., an active DB highwater offset)
/// falls cleanly on a frame boundary (`RB-09b` AC2).
fn verify_committed_frame_structure(
    file: &mut File,
    start_offset: u64,
    end_offset: u64,
    required_boundary: Option<u64>,
) -> StorageResult<()> {
    if start_offset > end_offset {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue committed frame scan start exceeds end boundary".to_string(),
        });
    }
    if start_offset == end_offset {
        if let Some(req) = required_boundary
            && req != start_offset
        {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue DB highwater does not align with committed frame boundary"
                    .to_string(),
            });
        }
        return Ok(());
    }
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| StorageBackendError::io("seek capture queue segment for frame scan", e))?;
    let mut decoder = FrameStreamDecoder::with_known_len(file, start_offset, end_offset);
    let mut saw_required = required_boundary.is_none_or(|req| req == start_offset);
    loop {
        match decoder.next_frame() {
            Ok(FrameStreamItem::Frame { frame, .. }) => {
                let _: CaptureBatchPayload =
                    decode_postcard_payload(&frame, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION).map_err(
                        |_| StorageBackendError::CorruptCommittedFrame {
                            detail: "committed capture queue frame failed postcard envelope decode"
                                .to_string(),
                        },
                    )?;
                if required_boundary == Some(decoder.offset()) {
                    saw_required = true;
                }
            }
            Ok(FrameStreamItem::CleanEof { offset: eof }) => {
                if eof != end_offset {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "committed capture queue segment ended before certified boundary"
                            .to_string(),
                    });
                }
                if required_boundary == Some(eof) {
                    saw_required = true;
                }
                break;
            }
            Err(_) => {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "committed capture queue frame failed CRC32C or structural validation"
                        .to_string(),
                });
            }
        }
    }
    if !saw_required {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue DB highwater does not align with a committed frame boundary"
                .to_string(),
        });
    }
    Ok(())
}

/// Authenticate a sealed segment (`min_live_seg <= seg_id < active_seg`)
/// against its `LTM2.sealed[]` record (`actual_len == sealed_len`,
/// `crc32c(seg[0..sealed_len]) == sealed_crc32c`, and full structural frame
/// validation, `ADR-RB-05` #4 ① & ②).
fn open_and_verify_sealed_segment(
    queue_root: &Path,
    lane: CaptureQueueLane,
    meta: &LaneTailMeta,
    seg_id: u32,
    required_boundary: Option<u64>,
) -> StorageResult<(File, u64)> {
    let sealed_entry = meta
        .sealed
        .iter()
        .find(|entry| entry.seg_id == seg_id)
        .ok_or_else(|| StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue sealed segment missing from LTM2 sealed table".to_string(),
        })?;
    if required_boundary.is_some_and(|req| req > sealed_entry.sealed_len) {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue DB highwater exceeds LTM2 sealed_len".to_string(),
        });
    }
    let (mut file, actual_len) = open_lane_segment_readonly(queue_root, lane, seg_id)?;
    if actual_len != sealed_entry.sealed_len {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue sealed segment length does not match LTM2 sealed_len"
                .to_string(),
        });
    }
    stream_verify_prefix_crc(
        &mut file,
        sealed_entry.sealed_len,
        sealed_entry.sealed_crc32c,
        "capture queue sealed segment CRC32C does not match LTM2 sealed_crc32c",
    )?;
    verify_committed_frame_structure(&mut file, 0, sealed_entry.sealed_len, required_boundary)?;
    Ok((file, sealed_entry.sealed_len))
}

/// Decode up to `remaining_budget` frames (bounded by `max_window` elapsed since
/// `batch_start` once `>= 1` frame is collected) strictly inside
/// `[start_offset, end_offset)`.
#[allow(clippy::too_many_arguments)]
fn read_batch_from_verified_segment_into(
    file: &mut File,
    start_offset: u64,
    end_offset: u64,
    remaining_budget: usize,
    batch_start: Instant,
    max_window: Duration,
    observer: &WriterObserver,
    items: &mut Vec<CaptureBatchItem>,
) -> StorageResult<u64> {
    if start_offset > end_offset {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue read cursor exceeds verified segment boundary".to_string(),
        });
    }
    if start_offset == end_offset || remaining_budget == 0 {
        return Ok(start_offset);
    }

    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| StorageBackendError::io("seek capture queue segment to highwater", e))?;
    let mut decoder = FrameStreamDecoder::with_known_len(file, start_offset, end_offset);
    let target_len = items.len().saturating_add(remaining_budget);

    while items.len() < target_len {
        if !items.is_empty()
            && !max_window.is_zero()
            && observer.window_elapsed(batch_start) >= max_window
        {
            break;
        }
        match decoder.next_frame() {
            Ok(FrameStreamItem::Frame { frame, .. }) => {
                let payload: CaptureBatchPayload =
                    decode_postcard_payload(&frame, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION).map_err(
                        |_| StorageBackendError::CorruptCommittedFrame {
                            detail: "committed capture queue frame failed postcard envelope decode"
                                .to_string(),
                        },
                    )?;
                items.push(CaptureBatchItem {
                    dedup_key: frame.dedup_key,
                    payload,
                });
            }
            Ok(FrameStreamItem::CleanEof { offset: eof }) => {
                if eof != end_offset {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "committed capture queue segment ended before certified boundary"
                            .to_string(),
                    });
                }
                break;
            }
            Err(_) => {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "committed capture queue frame failed CRC32C or structural validation"
                        .to_string(),
                });
            }
        }
    }

    Ok(decoder.offset())
}

/// Decode up to `max_batch_size` frames strictly inside `[start_offset, end_offset)`.
fn read_batch_from_verified_segment(
    file: &mut File,
    start_offset: u64,
    end_offset: u64,
    options: QueueWriterOptions,
    observer: &WriterObserver,
) -> StorageResult<(Vec<CaptureBatchItem>, u64)> {
    let max_items = options.effective_max_batch_size();
    let max_window = options.effective_max_batch_window();
    let mut items = Vec::with_capacity(max_items.min(128));
    let end_cursor = read_batch_from_verified_segment_into(
        file,
        start_offset,
        end_offset,
        max_items,
        Instant::now(),
        max_window,
        observer,
        &mut items,
    )?;
    Ok((items, end_cursor))
}

/// Non-mutating check whether `lane` currently has pending committed frames,
/// un-reclaimed segments, or uncommitted tail/orphan state relative to `hw`.
fn lane_has_pending_work(
    queue_root: &Path,
    lane: CaptureQueueLane,
    hw: CaptureQueueHighwater,
) -> StorageResult<bool> {
    match inspect_lane_readonly(queue_root, lane)? {
        ConsumerLaneSnapshot::Empty => Ok(hw != CaptureQueueHighwater::default()),
        ConsumerLaneSnapshot::Populated { meta, seg_ids } => {
            if meta.active_seg != hw.segment_id
                || meta.committed_len != hw.byte_offset
                || meta.min_live_seg < hw.segment_id
                || seg_ids.iter().any(|&seg| seg != meta.active_seg)
            {
                return Ok(true);
            }
            let (_file, actual_len) =
                open_lane_segment_readonly(queue_root, lane, meta.active_seg)?;
            Ok(actual_len != meta.committed_len)
        }
    }
}

enum LaneStepOutcome {
    /// Lane is completely caught up with zero pending frames, un-reclaimed
    /// segments, or uncommitted tails.
    CaughtUp,
    /// Committed one `CaptureStore::append_capture_batch` transaction (or
    /// reclaimed / recovered and needs another inspection step).
    Progressed,
}

/// Execute at most ONE batch commit (or recovery/reclaim step) on `lane` under
/// held `writer.lock`. Returns `CaughtUp` when `lane` has no remaining work.
#[allow(clippy::too_many_arguments)]
async fn step_lane_once(
    queue_root: &Path,
    lane: CaptureQueueLane,
    store: &dyn CaptureStore,
    options: QueueWriterOptions,
    hw_snapshot: &mut QueueHighwaterSnapshot,
    report: &mut CaptureDrainReport,
    writer_lock: &WriterLockGuard,
    observer: &mut WriterObserver,
    last_recovery_signature: &mut Option<(u32, u64, u64, u32, u32)>,
) -> StorageResult<LaneStepOutcome> {
    let hw = hw_snapshot.get(lane);
    let lane_state = inspect_lane_readonly(queue_root, lane)?;

    let (meta, seg_ids) = match lane_state {
        ConsumerLaneSnapshot::Empty => {
            if hw != CaptureQueueHighwater::default() {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "capture queue lane is empty while DB highwater is non-zero"
                        .to_string(),
                });
            }
            return Ok(LaneStepOutcome::CaughtUp);
        }
        ConsumerLaneSnapshot::Populated { meta, seg_ids } => (meta, seg_ids),
    };

    // Highwater / LTM2 monotonicity guard (`ADR-RB-05` #4 ①).
    if meta.active_seg < hw.segment_id
        || (meta.active_seg == hw.segment_id && meta.committed_len < hw.byte_offset)
    {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 metadata regressed behind authoritative DB highwater"
                .to_string(),
        });
    }
    if hw.segment_id < meta.min_live_seg {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 min_live_seg surpassed unconsumed DB highwater".to_string(),
        });
    }

    // Reclaim any already-consumed historical sealed segments (`min_live_seg <
    // hw.segment_id && min_live_seg < active_seg`) in `writer.lock -> append.lock`
    // order (`ADR-RB-05` #4 ③), authenticating the sealed boundary against
    // `LTM2.sealed[]` before touching disk (`RB-09d`).
    if meta.min_live_seg < hw.segment_id && meta.min_live_seg < meta.active_seg {
        let _ = open_and_verify_sealed_segment(queue_root, lane, &meta, meta.min_live_seg, None)?;
        let _reclaim = call_reclaim_sealed_segment(
            queue_root,
            lane,
            meta.min_live_seg,
            hw.segment_id,
            writer_lock,
            observer,
        )
        .await?;
        report.reclaimed_segments += 1;
        *last_recovery_signature = None;
        return Ok(LaneStepOutcome::Progressed);
    }

    // Clean up any post-LTM2 crash orphan segment (`seg < min_live_seg`) via
    // `writer.lock -> append.lock` recovery delegation (`RB-09d` AC5).
    let has_reclaimed_orphan = seg_ids.iter().any(|&seg| seg < meta.min_live_seg);
    let max_live_seg = seg_ids
        .iter()
        .copied()
        .filter(|&seg| seg >= meta.min_live_seg)
        .max()
        .ok_or_else(|| StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue lane has no live segment files >= min_live_seg".to_string(),
        })?;
    if max_live_seg < meta.active_seg || seg_ids.binary_search(&meta.active_seg).is_err() {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue active segment is missing from lane directory".to_string(),
        });
    }

    // Branch A: `hw.segment_id < meta.active_seg` (sealed rotated segment).
    if hw.segment_id < meta.active_seg {
        let (mut seg_file, sealed_len) = open_and_verify_sealed_segment(
            queue_root,
            lane,
            &meta,
            hw.segment_id,
            Some(hw.byte_offset),
        )?;
        if hw.byte_offset > sealed_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue DB highwater exceeds LTM2 sealed_len".to_string(),
            });
        }

        // Rule ② (c): active segment was drained to `(S, committed_len)` and
        // subsequently rotated -> authenticate `LTM2.sealed[S]` (done above)
        // and execute atomic zero-frame advance to `(S + 1, 0)`.
        if hw.byte_offset == sealed_len {
            if lane == CaptureQueueLane::Normal {
                observer.notify_sealed_pre_commit(hw.segment_id);
                if lane_has_pending_work(
                    queue_root,
                    CaptureQueueLane::Priority,
                    hw_snapshot.get(CaptureQueueLane::Priority),
                )? {
                    return Ok(LaneStepOutcome::Progressed);
                }
            }
            let next_hw = compute_next_highwater(hw, &meta, sealed_len)?;
            #[cfg(test)]
            observer.maybe_crash(WriterCrashPoint::BeforeEmptyAdvanceCommit)?;
            let outcome = store
                .append_capture_batch(&[], Some((lane, next_hw)))
                .await?;
            hw_snapshot.set(lane, outcome.highwater.unwrap_or(next_hw));
            match lane {
                CaptureQueueLane::Priority => report.priority_batches += 1,
                CaptureQueueLane::Normal => report.normal_batches += 1,
            }
            observer.maybe_crash(crash_after_commit_before_mirror())?;
            write_highwater_mirror(queue_root, hw_snapshot)?;

            // Eagerly reclaim `S` immediately after DB highwater crossed past it.
            if hw.segment_id == meta.min_live_seg {
                let _reclaim = call_reclaim_sealed_segment(
                    queue_root,
                    lane,
                    hw.segment_id,
                    hw_snapshot.get(lane).segment_id,
                    writer_lock,
                    observer,
                )
                .await?;
                report.reclaimed_segments += 1;
            }
            *last_recovery_signature = None;
            return Ok(LaneStepOutcome::Progressed);
        }

        // Rule ② (b): read up to 128 frames within `[hw.byte_offset, sealed_len)`.
        let (items, batch_end_offset) = read_batch_from_verified_segment(
            &mut seg_file,
            hw.byte_offset,
            sealed_len,
            options,
            observer,
        )?;
        if items.is_empty() {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue sealed segment produced zero frames before sealed_len"
                    .to_string(),
            });
        }
        // Strict AC1 preemption guard before committing a sealed `normal/`
        // batch: if `priority/` received work while authenticating or decoding
        // this rotated segment, yield so the outer loop commits `priority/` FIRST.
        if lane == CaptureQueueLane::Normal {
            observer.notify_sealed_pre_commit(hw.segment_id);
            if lane_has_pending_work(
                queue_root,
                CaptureQueueLane::Priority,
                hw_snapshot.get(CaptureQueueLane::Priority),
            )? {
                return Ok(LaneStepOutcome::Progressed);
            }
        }
        let next_hw = compute_next_highwater(hw, &meta, batch_end_offset)?;
        let outcome = store
            .append_capture_batch(&items, Some((lane, next_hw)))
            .await?;
        hw_snapshot.set(lane, outcome.highwater.unwrap_or(next_hw));
        report.applied_frames += outcome.applied_count;
        report.deduplicated_frames += outcome.deduplicated_count;
        match lane {
            CaptureQueueLane::Priority => report.priority_batches += 1,
            CaptureQueueLane::Normal => report.normal_batches += 1,
        }
        observer.maybe_crash(crash_after_commit_before_mirror())?;
        write_highwater_mirror(queue_root, hw_snapshot)?;

        // If this final batch crossed from sealed `S` to `(S + 1, 0)`, eagerly
        // reclaim `S` right now (`ADR-RB-05` #4 ③, `RB-09` AC5, `RB-09l`).
        let committed_hw = hw_snapshot.get(lane);
        if committed_hw.segment_id > hw.segment_id && hw.segment_id == meta.min_live_seg {
            let _reclaim = call_reclaim_sealed_segment(
                queue_root,
                lane,
                hw.segment_id,
                committed_hw.segment_id,
                writer_lock,
                observer,
            )
            .await?;
            report.reclaimed_segments += 1;
        }
        *last_recovery_signature = None;
        return Ok(LaneStepOutcome::Progressed);
    }

    // Branch B: `hw.segment_id == meta.active_seg` (active segment).
    let (mut active_file, actual_len) =
        open_lane_segment_readonly(queue_root, lane, meta.active_seg)?;
    if actual_len < meta.committed_len {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue active segment is shorter than LTM2 committed_len".to_string(),
        });
    }
    stream_verify_prefix_crc(
        &mut active_file,
        meta.committed_len,
        meta.prefix_crc32c,
        "capture queue active segment prefix CRC32C mismatch",
    )?;
    verify_committed_frame_structure(
        &mut active_file,
        0,
        meta.committed_len,
        Some(hw.byte_offset),
    )?;

    // Rule ② (a): consume committed frames strictly inside `[hw.byte_offset, committed_len)`.
    // For `normal/`, coalesce newly committed arriving frames up to `max_batch_size`
    // (128) or `max_batch_window` (50ms) into ONE single write transaction while
    // yielding immediately if `priority/` receives work (`ADR-RB-05` #4, `RB-09` AC1/AC3).
    if hw.byte_offset < meta.committed_len {
        let batch_start = Instant::now();
        let max_items = options.effective_max_batch_size();
        let max_window = options.effective_max_batch_window();
        let mut current_meta = meta;
        let mut items = Vec::with_capacity(max_items.min(128));
        let mut current_offset = read_batch_from_verified_segment_into(
            &mut active_file,
            hw.byte_offset,
            current_meta.committed_len,
            max_items,
            batch_start,
            max_window,
            observer,
            &mut items,
        )?;
        if items.is_empty() {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue active segment produced zero frames before committed_len"
                    .to_string(),
            });
        }

        if lane == CaptureQueueLane::Normal && items.len() < max_items && !max_window.is_zero() {
            while items.len() < max_items {
                if lane_has_pending_work(
                    queue_root,
                    CaptureQueueLane::Priority,
                    hw_snapshot.get(CaptureQueueLane::Priority),
                )? {
                    // Priority preempts BEFORE committing the pending `normal/`
                    // batch so `priority/` strictly commits first (`RB-09` AC1).
                    return Ok(LaneStepOutcome::Progressed);
                }
                observer.notify_window_poll(items.len());
                if lane_has_pending_work(
                    queue_root,
                    CaptureQueueLane::Priority,
                    hw_snapshot.get(CaptureQueueLane::Priority),
                )? {
                    return Ok(LaneStepOutcome::Progressed);
                }
                let timed_out = observer.window_elapsed(batch_start) >= max_window;
                let refreshed = inspect_lane_readonly(queue_root, CaptureQueueLane::Normal)?;
                let ConsumerLaneSnapshot::Populated {
                    meta: next_meta, ..
                } = refreshed
                else {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "capture queue lane became empty during active batch collection"
                            .to_string(),
                    });
                };
                if next_meta.active_seg < current_meta.active_seg
                    || (next_meta.active_seg == current_meta.active_seg
                        && next_meta.committed_len < current_meta.committed_len)
                {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail:
                            "capture queue LTM2 metadata regressed during active batch collection"
                                .to_string(),
                    });
                }
                // If a producer rotated the active segment mid-window, commit
                // the contiguous prefix collected on `current_meta.active_seg`
                // now; the next iteration authenticates `LTM2.sealed[]` cleanly.
                if next_meta.active_seg != current_meta.active_seg {
                    break;
                }
                if next_meta.committed_len > current_meta.committed_len {
                    let latest_len = active_file
                        .metadata()
                        .map_err(|e| StorageBackendError::io("stat capture queue segment", e))?
                        .len();
                    if latest_len < next_meta.committed_len {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail:
                                "capture queue active segment is shorter than LTM2 committed_len"
                                    .to_string(),
                        });
                    }
                    stream_verify_prefix_crc(
                        &mut active_file,
                        next_meta.committed_len,
                        next_meta.prefix_crc32c,
                        "capture queue active segment prefix CRC32C mismatch",
                    )?;
                    verify_committed_frame_structure(
                        &mut active_file,
                        current_meta.committed_len,
                        next_meta.committed_len,
                        None,
                    )?;
                    let remaining_budget = max_items.saturating_sub(items.len());
                    current_offset = read_batch_from_verified_segment_into(
                        &mut active_file,
                        current_offset,
                        next_meta.committed_len,
                        remaining_budget,
                        batch_start,
                        max_window,
                        observer,
                        &mut items,
                    )?;
                    current_meta = next_meta;
                    if items.len() >= max_items {
                        break;
                    }
                    if timed_out {
                        break;
                    }
                    continue;
                }
                if timed_out {
                    break;
                }
                let Some(remaining) = max_window.checked_sub(observer.window_elapsed(batch_start))
                else {
                    break;
                };
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(remaining.min(Duration::from_millis(2))).await;
            }
        }

        // Final strict AC1 preemption guard immediately before committing a
        // `normal/` DB write transaction: if `priority/` received work at any
        // point while preparing this batch, yield cleanly so the outer loop
        // commits `priority/` FIRST.
        if lane == CaptureQueueLane::Normal
            && lane_has_pending_work(
                queue_root,
                CaptureQueueLane::Priority,
                hw_snapshot.get(CaptureQueueLane::Priority),
            )?
        {
            return Ok(LaneStepOutcome::Progressed);
        }

        let next_hw = compute_next_highwater(hw, &current_meta, current_offset)?;
        let outcome = store
            .append_capture_batch(&items, Some((lane, next_hw)))
            .await?;
        hw_snapshot.set(lane, outcome.highwater.unwrap_or(next_hw));
        report.applied_frames += outcome.applied_count;
        report.deduplicated_frames += outcome.deduplicated_count;
        match lane {
            CaptureQueueLane::Priority => report.priority_batches += 1,
            CaptureQueueLane::Normal => report.normal_batches += 1,
        }
        observer.maybe_crash(crash_after_commit_before_mirror())?;
        write_highwater_mirror(queue_root, hw_snapshot)?;
        *last_recovery_signature = None;
        return Ok(LaneStepOutcome::Progressed);
    }

    // Caught up to `(active_seg, committed_len)`: check whether uncommitted
    // tail bytes (`actual_len > committed_len`), a Case D successor segment
    // (`max_live_seg > active_seg`), or a post-reclaim crash orphan requires
    // `writer.lock -> append.lock` delegation (`ADR-RB-05` #4 ②).
    if actual_len > meta.committed_len || max_live_seg > meta.active_seg || has_reclaimed_orphan {
        let min_disk_seg = seg_ids.first().copied().unwrap_or(0);
        let sig = (
            meta.active_seg,
            meta.committed_len,
            actual_len,
            max_live_seg,
            min_disk_seg,
        );
        if *last_recovery_signature == Some(sig) {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue lane recovery did not converge uncommitted state"
                    .to_string(),
            });
        }
        *last_recovery_signature = Some(sig);
        drop(active_file);
        // Delegates under `writer.lock -> capture-queue/append.lock` without
        // modifying DB `QUEUE_HIGHWATER_*` or the external `highwater` mirror.
        let _rec = call_recover_lane(queue_root, lane, writer_lock, observer).await?;
        report.recovery_delegations += 1;
        return Ok(LaneStepOutcome::Progressed);
    }

    *last_recovery_signature = None;
    Ok(LaneStepOutcome::CaughtUp)
}

/// Strictly read-only preflight validation of a lane's metadata, highwater
/// monotonicity, EVERY un-reclaimed rotated segment (`min_live_seg <= seg <
/// active_seg`) against `LTM2.sealed[]`, and active committed prefix BEFORE
/// any DB write transaction or mirror update runs on either lane
/// (`RB-09b` AC2–AC5, `RB-09d` AC1–AC4).
fn preflight_validate_lane(
    queue_root: &Path,
    lane: CaptureQueueLane,
    hw: CaptureQueueHighwater,
) -> StorageResult<()> {
    let (meta, seg_ids) = match inspect_lane_readonly(queue_root, lane)? {
        ConsumerLaneSnapshot::Empty => {
            if hw != CaptureQueueHighwater::default() {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "capture queue lane is empty while DB highwater is non-zero"
                        .to_string(),
                });
            }
            return Ok(());
        }
        ConsumerLaneSnapshot::Populated { meta, seg_ids } => (meta, seg_ids),
    };

    if meta.active_seg < hw.segment_id
        || (meta.active_seg == hw.segment_id && meta.committed_len < hw.byte_offset)
    {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 metadata regressed behind authoritative DB highwater"
                .to_string(),
        });
    }
    if hw.segment_id < meta.min_live_seg {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 min_live_seg surpassed unconsumed DB highwater".to_string(),
        });
    }
    if seg_ids.binary_search(&meta.active_seg).is_err() {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue active segment is missing from lane directory".to_string(),
        });
    }

    // Authenticate EVERY un-reclaimed rotated segment `min_live_seg <= seg < active_seg`
    // against `LTM2.sealed[]` (`ADR-RB-05` #4, `RB-09d` AC1–AC4). Post-reclaim
    // crash orphans (`seg < min_live_seg`) are intentionally outside this range
    // so recovery safely unlinks them without false alarms (`RB-09d` AC5).
    for seg_id in meta.min_live_seg..meta.active_seg {
        let req = if seg_id == hw.segment_id {
            Some(hw.byte_offset)
        } else {
            None
        };
        let (_f, sealed_len) =
            open_and_verify_sealed_segment(queue_root, lane, &meta, seg_id, req)?;
        if seg_id == hw.segment_id && hw.byte_offset > sealed_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue DB highwater exceeds LTM2 sealed_len".to_string(),
            });
        }
    }

    let (mut active_file, actual_len) =
        open_lane_segment_readonly(queue_root, lane, meta.active_seg)?;
    if actual_len < meta.committed_len {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue active segment is shorter than LTM2 committed_len".to_string(),
        });
    }
    stream_verify_prefix_crc(
        &mut active_file,
        meta.committed_len,
        meta.prefix_crc32c,
        "capture queue active segment prefix CRC32C mismatch",
    )?;
    let active_req = if hw.segment_id == meta.active_seg {
        Some(hw.byte_offset)
    } else {
        None
    };
    verify_committed_frame_structure(&mut active_file, 0, meta.committed_len, active_req)?;
    Ok(())
}

async fn drain_locked_at_root(
    queue_root: &Path,
    store: &dyn CaptureStore,
    options: QueueWriterOptions,
    writer_lock: &WriterLockGuard,
    observer: &mut WriterObserver,
) -> StorageResult<CaptureDrainReport> {
    // Load authoritative DB highwater read-only and validate BOTH lanes
    // read-only BEFORE executing ANY DB transaction or mirror update so a
    // corrupt `normal/` lane fails closed before `priority/` commits (and vice versa).
    let mut hw_snapshot = load_authoritative_highwater(store).await?;
    preflight_validate_lane(queue_root, CaptureQueueLane::Priority, hw_snapshot.priority)?;
    preflight_validate_lane(queue_root, CaptureQueueLane::Normal, hw_snapshot.normal)?;
    let mut report = CaptureDrainReport {
        acquired_writer_lock: true,
        priority_batches: 0,
        normal_batches: 0,
        applied_frames: 0,
        deduplicated_frames: 0,
        reclaimed_segments: 0,
        recovery_delegations: 0,
        highwater: None,
    };

    let mut priority_recovery_sig = None;
    let mut normal_recovery_sig = None;

    loop {
        // Strict priority-first invariant: drain `priority/` completely before
        // taking each batch from `normal/`.
        while matches!(
            step_lane_once(
                queue_root,
                CaptureQueueLane::Priority,
                store,
                options,
                &mut hw_snapshot,
                &mut report,
                writer_lock,
                observer,
                &mut priority_recovery_sig,
            )
            .await?,
            LaneStepOutcome::Progressed
        ) {}

        match step_lane_once(
            queue_root,
            CaptureQueueLane::Normal,
            store,
            options,
            &mut hw_snapshot,
            &mut report,
            writer_lock,
            observer,
            &mut normal_recovery_sig,
        )
        .await?
        {
            LaneStepOutcome::Progressed => {
                // Re-loop to re-check `priority/` before consuming the next `normal/` batch.
                continue;
            }
            LaneStepOutcome::CaughtUp => break,
        }
    }

    // Both lanes converged cleanly: ensure the advisory external mirror agrees
    // with authoritative DB highwater (self-healing stale/absent/corrupt
    // mirrors even on a 0-new-frame restart after `AfterDbCommitBeforeMirror`).
    if read_highwater_mirror(queue_root)? != Some(hw_snapshot) {
        write_highwater_mirror(queue_root, &hw_snapshot)?;
    }

    report.highwater = Some(hw_snapshot);
    Ok(report)
}

// ============================================================================
// Unix Descriptor-Pinned Lock & Read-Only Inspection Helpers
// ============================================================================

#[cfg(unix)]
mod unix_writer {
    use std::{
        ffi::{CStr, CString, OsStr},
        os::unix::{
            ffi::OsStrExt as _,
            io::{AsRawFd as _, FromRawFd as _, OwnedFd},
        },
    };

    use super::*;
    use crate::storage::queue::{
        QUEUE_DIR_MODE, QUEUE_FILE_MODE, map_queue_owner_mismatch_err, map_queue_symlink_err,
    };

    fn current_euid() -> u32 {
        // SAFETY: `geteuid` is always safe and side-effect-free.
        unsafe { libc::geteuid() }
    }

    fn osstr_to_cstring(s: &OsStr, context: &'static str) -> StorageResult<CString> {
        CString::new(s.as_bytes()).map_err(|_| {
            StorageBackendError::io(
                context,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path component contains interior NUL byte",
                ),
            )
        })
    }

    fn fstat_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `fd` is a valid open descriptor.
        let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        // SAFETY: `fstat` succeeded.
        Ok(unsafe { st.assume_init() })
    }

    fn fsync_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<()> {
        // SAFETY: `fd` is a valid open descriptor.
        let rc = unsafe { libc::fsync(fd.as_raw_fd()) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        Ok(())
    }

    fn clear_errno() {
        #[cfg(target_os = "macos")]
        // SAFETY: thread-local errno pointer is valid.
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        // SAFETY: thread-local errno pointer is valid.
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {}
    }

    fn is_symlink_errno_at(err: &io::Error, dir_fd: &OwnedFd, cname: &CStr) -> bool {
        let Some(code) = err.raw_os_error() else {
            return false;
        };
        if code == libc::ELOOP {
            return true;
        }
        if code == libc::EMLINK || code == libc::EFTYPE || code == libc::ENOTDIR {
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    dir_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc == 0 {
                // SAFETY: `fstatat` succeeded.
                let st = unsafe { st.assume_init() };
                return (st.st_mode & libc::S_IFMT) == libc::S_IFLNK;
            }
        }
        false
    }

    fn is_symlink_errno_path(err: &io::Error, path: &Path) -> bool {
        let Some(code) = err.raw_os_error() else {
            return false;
        };
        if code == libc::ELOOP {
            return true;
        }
        if code == libc::EMLINK || code == libc::EFTYPE || code == libc::ENOTDIR {
            return std::fs::symlink_metadata(path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
        }
        false
    }

    fn open_verified_dir(
        path: &Path,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<OwnedFd> {
        let cpath = osstr_to_cstring(path.as_os_str(), "open capture queue directory")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `cpath` is NUL-terminated.
        let raw_fd = unsafe { libc::open(cpath.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_path(&err, path) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io("open capture queue directory", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat capture queue directory")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a directory"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        let mode = u32::from(st.st_mode) & 0o777;
        if mode != QUEUE_DIR_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} permissions are too permissive (expected 0700)"),
            });
        }
        Ok(fd)
    }

    fn open_verified_subdir_os(
        parent_fd: &OwnedFd,
        name: &OsStr,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<OwnedFd> {
        let cname = osstr_to_cstring(name, "open capture queue lane directory")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `parent_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(parent_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, parent_fd, &cname) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io(
                "open capture queue lane directory",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat capture queue lane directory")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a directory"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        let mode = u32::from(st.st_mode) & 0o777;
        if mode != QUEUE_DIR_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} permissions are too permissive (expected 0700)"),
            });
        }
        Ok(fd)
    }

    fn open_verified_subdir(
        parent_fd: &OwnedFd,
        name: &str,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<OwnedFd> {
        open_verified_subdir_os(parent_fd, OsStr::new(name), expected_uid, subject)
    }

    fn open_verified_queue_root(queue_root: &Path, expected_uid: u32) -> StorageResult<OwnedFd> {
        if let Some(parent) = queue_root.parent().filter(|p| !p.as_os_str().is_empty())
            && parent.file_name().and_then(|n| n.to_str()) == Some(PRIVATE_SUBDIR)
            && let Some(root_name) = queue_root.file_name()
        {
            let private_fd =
                open_verified_dir(parent, expected_uid, "capture queue private directory")?;
            return open_verified_subdir_os(
                &private_fd,
                root_name,
                expected_uid,
                "capture queue root directory",
            );
        }
        open_verified_dir(queue_root, expected_uid, "capture queue root directory")
    }

    fn verify_lock_file_fd(fd: &OwnedFd, expected_uid: u32) -> StorageResult<()> {
        let st = fstat_fd(fd, "stat capture queue writer.lock")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue writer.lock is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue writer.lock"));
        }
        let mode = u32::from(st.st_mode) & 0o777;
        if mode != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue writer.lock permissions are too permissive (expected 0600)"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Open or exclusively create `writer.lock` without using `O_CREAT | O_NOFOLLOW`
    /// together without `O_EXCL` (avoids macOS XNU concurrent `ENOENT` quirk).
    fn open_or_create_writer_lock_fd(
        root_fd: &OwnedFd,
        expected_uid: u32,
    ) -> StorageResult<OwnedFd> {
        let cname = osstr_to_cstring(
            OsStr::new(QUEUE_WRITER_LOCK_FILE),
            "open capture queue writer.lock",
        )?;
        for _ in 0..8 {
            let open_flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
            // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
            let raw_fd = unsafe { libc::openat(root_fd.as_raw_fd(), cname.as_ptr(), open_flags) };
            if raw_fd >= 0 {
                // SAFETY: `raw_fd >= 0` is exclusively owned here.
                let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
                verify_lock_file_fd(&fd, expected_uid)?;
                return Ok(fd);
            }
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue writer.lock"));
            }
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "open capture queue writer.lock",
                    err,
                ));
            }

            let create_flags =
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
            let created_raw =
                unsafe { libc::openat(root_fd.as_raw_fd(), cname.as_ptr(), create_flags, 0o600) };
            if created_raw >= 0 {
                // SAFETY: `created_raw >= 0` is exclusively owned here.
                let fd = unsafe { OwnedFd::from_raw_fd(created_raw) };
                // SAFETY: `fd` is valid.
                if unsafe { libc::fchmod(fd.as_raw_fd(), QUEUE_FILE_MODE as libc::mode_t) } != 0 {
                    return Err(StorageBackendError::io(
                        "chmod capture queue writer.lock",
                        io::Error::last_os_error(),
                    ));
                }
                verify_lock_file_fd(&fd, expected_uid)?;
                fsync_fd(
                    root_fd,
                    "fsync capture queue root after creating writer.lock",
                )?;
                return Ok(fd);
            }
            let create_err = io::Error::last_os_error();
            if create_err.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            if is_symlink_errno_at(&create_err, root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue writer.lock"));
            }
            return Err(StorageBackendError::io(
                "create capture queue writer.lock",
                create_err,
            ));
        }
        Err(StorageBackendError::BackendInternal {
            detail: "exceeded retry budget opening capture queue writer.lock".to_string(),
        })
    }

    pub(super) fn try_acquire_writer_lock_unix(
        queue_root: &Path,
    ) -> StorageResult<Option<WriterLockGuard>> {
        let expected_uid = current_euid();
        let root_fd = open_verified_queue_root(queue_root, expected_uid)?;
        let lock_fd = open_or_create_writer_lock_fd(&root_fd, expected_uid)?;
        loop {
            // SAFETY: `lock_fd` is a valid open file descriptor.
            let rc = unsafe { libc::flock(lock_fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Some(WriterLockGuard {
                    _file: std::sync::Arc::new(File::from(lock_fd)),
                }));
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(StorageBackendError::io(
                "acquire capture queue writer.lock",
                err,
            ));
        }
    }

    pub(super) fn is_writer_active_unix(queue_root: &Path) -> StorageResult<bool> {
        let expected_uid = current_euid();
        let root_fd = match open_verified_queue_root(queue_root, expected_uid) {
            Ok(fd) => fd,
            Err(StorageBackendError::Io {
                kind: io::ErrorKind::NotFound,
                ..
            }) => return Ok(false),
            Err(err) => return Err(err),
        };
        let cname = osstr_to_cstring(
            OsStr::new(QUEUE_WRITER_LOCK_FILE),
            "probe capture queue writer.lock",
        )?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(root_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(false);
            }
            if is_symlink_errno_at(&err, &root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue writer.lock"));
            }
            return Err(StorageBackendError::io(
                "probe capture queue writer.lock",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let lock_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        verify_lock_file_fd(&lock_fd, expected_uid)?;
        loop {
            // SAFETY: `lock_fd` is valid.
            let rc = unsafe { libc::flock(lock_fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                // Lock was free; immediately release on drop.
                return Ok(false);
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(true);
            }
            return Err(StorageBackendError::io(
                "probe capture queue writer.lock",
                err,
            ));
        }
    }

    struct DirStreamGuard {
        ptr: *mut libc::DIR,
    }

    impl Drop for DirStreamGuard {
        fn drop(&mut self) {
            if !self.ptr.is_null() {
                // SAFETY: `self.ptr` was returned by `fdopendir`.
                unsafe {
                    libc::closedir(self.ptr);
                }
            }
        }
    }

    fn scan_lane_segment_ids_unix(lane_fd: &OwnedFd) -> StorageResult<Vec<u32>> {
        // SAFETY: `lane_fd` is a valid open directory descriptor.
        let dup_raw = unsafe { libc::fcntl(lane_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_raw < 0 {
            return Err(StorageBackendError::io(
                "duplicate capture queue lane descriptor",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `dup_raw` is valid; `fdopendir` takes ownership on success.
        let dir_ptr = unsafe { libc::fdopendir(dup_raw) };
        if dir_ptr.is_null() {
            let err = io::Error::last_os_error();
            // SAFETY: `fdopendir` failed so we still own `dup_raw`.
            unsafe {
                libc::close(dup_raw);
            }
            return Err(StorageBackendError::io(
                "open capture queue lane directory stream",
                err,
            ));
        }
        let guard = DirStreamGuard { ptr: dir_ptr };
        // SAFETY: `guard.ptr` is valid; rewind shared directory offset.
        unsafe {
            libc::rewinddir(guard.ptr);
        }
        let mut seg_ids = Vec::new();
        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is valid.
            let entry = unsafe { libc::readdir(guard.ptr) };
            if entry.is_null() {
                let err = io::Error::last_os_error();
                if err.raw_os_error().unwrap_or(0) != 0 {
                    return Err(StorageBackendError::io(
                        "read capture queue lane directory entry",
                        err,
                    ));
                }
                break;
            }
            // SAFETY: `entry` is non-null and `d_name` is NUL-terminated.
            let name_bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            if matches_segment_log_glob_bytes(name_bytes) {
                match parse_segment_log_file_name(name_bytes) {
                    Some(seg_id) => seg_ids.push(seg_id),
                    None => {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail: "malformed events-*.log segment filename in capture queue lane"
                                .to_string(),
                        });
                    }
                }
            }
        }
        seg_ids.sort_unstable();
        seg_ids.dedup();
        Ok(seg_ids)
    }

    fn read_meta_replica_unix(
        lane_fd: &OwnedFd,
        file_name: &str,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<ReplicaRead> {
        let cname = osstr_to_cstring(OsStr::new(file_name), "open capture queue metadata")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(ReplicaRead::Absent);
            }
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io("open capture queue metadata", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat capture queue metadata")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: format!("{subject} is not a regular file"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        let Ok(file_len) = u64::try_from(st.st_size) else {
            return Ok(ReplicaRead::Corrupt);
        };
        if file_len > LTM2_MAX_LEN as u64 {
            return Ok(ReplicaRead::Corrupt);
        }
        let file = File::from(fd);
        let mut bytes = Vec::with_capacity((file_len as usize).min(LTM2_MAX_LEN + 1));
        file.take((LTM2_MAX_LEN + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| StorageBackendError::io("read capture queue metadata", e))?;
        if bytes.len() > LTM2_MAX_LEN {
            return Ok(ReplicaRead::Corrupt);
        }
        match LaneTailMeta::decode(&bytes) {
            Ok(meta) => Ok(ReplicaRead::Valid(meta)),
            Err(_) => Ok(ReplicaRead::Corrupt),
        }
    }

    pub(super) fn inspect_lane_readonly_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
    ) -> StorageResult<ConsumerLaneSnapshot> {
        let expected_uid = current_euid();
        let root_fd = open_verified_queue_root(queue_root, expected_uid)?;
        let lane_fd = open_verified_subdir(
            &root_fd,
            lane_subdir_name(lane),
            expected_uid,
            "capture queue lane directory",
        )?;
        // Read LTM2 replicas BEFORE scanning segment files so a concurrent
        // producer creating segment 0 and committing LTM2 cannot cause a
        // false-positive Case 0c (`seg_ids == []` with non-empty LTM2).
        let primary = read_meta_replica_unix(
            &lane_fd,
            QUEUE_TAIL_META_FILE,
            expected_uid,
            "capture queue tail.meta",
        )?;
        let backup = read_meta_replica_unix(
            &lane_fd,
            QUEUE_TAIL_META_BAK_FILE,
            expected_uid,
            "capture queue tail.meta.bak",
        )?;
        let mut resolved = resolve_consumer_replicas(primary, backup)?;
        let seg_ids = scan_lane_segment_ids_unix(&lane_fd)?;

        // Guard against a concurrent virgin Case 0a bootstrap completing
        // between our initial replica read and `scan_lane_segment_ids_unix`.
        if resolved.is_none() && !seg_ids.is_empty() {
            let primary_retry = read_meta_replica_unix(
                &lane_fd,
                QUEUE_TAIL_META_FILE,
                expected_uid,
                "capture queue tail.meta",
            )?;
            let backup_retry = read_meta_replica_unix(
                &lane_fd,
                QUEUE_TAIL_META_BAK_FILE,
                expected_uid,
                "capture queue tail.meta.bak",
            )?;
            resolved = resolve_consumer_replicas(primary_retry, backup_retry)?;
        }

        if seg_ids.is_empty() {
            return match resolved {
                None => Ok(ConsumerLaneSnapshot::Empty),
                Some(meta) if meta.is_initial_empty() => Ok(ConsumerLaneSnapshot::Empty),
                Some(_) => Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "Case 0c: zero segment files present while LTM2 records non-empty committed state"
                        .to_string(),
                }),
            };
        }

        let Some(meta) = resolved else {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail:
                    "Subcase B2: segment files exist while both LTM2 tail.meta replicas are absent"
                        .to_string(),
            });
        };
        Ok(ConsumerLaneSnapshot::Populated { meta, seg_ids })
    }

    pub(super) fn open_lane_segment_readonly_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
    ) -> StorageResult<(File, u64)> {
        let expected_uid = current_euid();
        let root_fd = open_verified_queue_root(queue_root, expected_uid)?;
        let lane_fd = open_verified_subdir(
            &root_fd,
            lane_subdir_name(lane),
            expected_uid,
            "capture queue lane directory",
        )?;
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open capture queue segment")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, &lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            if err.kind() == io::ErrorKind::NotFound {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "capture queue segment file is missing".to_string(),
                });
            }
            return Err(StorageBackendError::io("open capture queue segment", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat capture queue segment")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment path is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue segment file"));
        }
        let actual_len =
            u64::try_from(st.st_size).map_err(|_| StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment has negative size".to_string(),
            })?;
        Ok((File::from(fd), actual_len))
    }
}

// ============================================================================
// Non-Unix (Windows) Lock & Read-Only Inspection Helpers
// ============================================================================

#[cfg(not(unix))]
mod non_unix_writer {
    use super::*;

    #[cfg(windows)]
    mod win_writer {
        use std::{
            fs,
            os::windows::{
                ffi::OsStrExt as _,
                io::{AsRawHandle as _, FromRawHandle as _},
            },
        };

        use windows_sys::Win32::{
            Foundation::{
                CloseHandle, ERROR_LOCK_VIOLATION, GENERIC_READ, GENERIC_WRITE, HANDLE,
                INVALID_HANDLE_VALUE,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
                LockFileEx, OPEN_ALWAYS, OPEN_EXISTING,
            },
            System::IO::OVERLAPPED,
        };

        use super::*;
        use crate::storage::queue::map_queue_symlink_err;

        struct WinHandleGuard(HANDLE);

        impl Drop for WinHandleGuard {
            fn drop(&mut self) {
                // SAFETY: `self.0` is a valid Win32 handle opened by `CreateFileW`.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }

        fn to_wide_nul(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        fn file_info_by_handle(
            handle: HANDLE,
            context: &'static str,
        ) -> StorageResult<BY_HANDLE_FILE_INFORMATION> {
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
            // SAFETY: `handle` is valid and `info` points to writable memory.
            let ok = unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) };
            if ok == 0 {
                return Err(StorageBackendError::io(context, io::Error::last_os_error()));
            }
            // SAFETY: `GetFileInformationByHandle` succeeded and initialized `info`.
            Ok(unsafe { info.assume_init() })
        }

        fn verify_handle_regular_file(handle: HANDLE, subject: &'static str) -> StorageResult<()> {
            let info = file_info_by_handle(handle, "stat capture queue handle")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: format!("{subject} is not a regular file"),
                });
            }
            Ok(())
        }

        /// Open and PIN a Windows directory handle with
        /// `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT` and
        /// `FILE_SHARE_READ | FILE_SHARE_WRITE` (explicitly omitting
        /// `FILE_SHARE_DELETE`), verifying directly on the opened handle that it
        /// is a real non-reparse directory. Holding the returned
        /// [`WinHandleGuard`] prevents directory rename/delete/junction-swap
        /// across the entire operation.
        fn pin_dir_not_reparse(dir: &Path, subject: &'static str) -> StorageResult<WinHandleGuard> {
            let wide = to_wide_nul(dir);
            // SAFETY: `wide` is NUL-terminated. Omitting `FILE_SHARE_DELETE`
            // prevents concurrent directory rename/deletion or junction swap
            // while the handle remains open.
            let handle: HANDLE = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(StorageBackendError::io(
                    "open capture queue directory",
                    io::Error::last_os_error(),
                ));
            }
            let guard = WinHandleGuard(handle);
            let info = file_info_by_handle(guard.0, "stat capture queue directory handle")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} is not a directory"),
                });
            }
            Ok(guard)
        }

        fn pin_queue_root_chain(
            queue_root: &Path,
        ) -> StorageResult<(Option<WinHandleGuard>, WinHandleGuard)> {
            let private_pin = if let Some(parent) =
                queue_root.parent().filter(|p| !p.as_os_str().is_empty())
                && parent.file_name().and_then(|n| n.to_str()) == Some(PRIVATE_SUBDIR)
            {
                Some(pin_dir_not_reparse(
                    parent,
                    "capture queue private directory",
                )?)
            } else {
                None
            };
            let root_pin = pin_dir_not_reparse(queue_root, "capture queue root directory")?;
            Ok((private_pin, root_pin))
        }

        fn open_nofollow_file(
            path: &Path,
            access: u32,
            share_mode: u32,
            creation: u32,
            subject: &'static str,
            context: &'static str,
        ) -> StorageResult<File> {
            let wide = to_wide_nul(path);
            // SAFETY: `wide` is NUL-terminated.
            let handle: HANDLE = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    share_mode,
                    std::ptr::null(),
                    creation,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(StorageBackendError::io(context, io::Error::last_os_error()));
            }
            // SAFETY: `handle` is exclusively owned here and validated immediately via handle.
            let file = unsafe { File::from_raw_handle(handle as *mut std::ffi::c_void) };
            verify_handle_regular_file(file.as_raw_handle() as HANDLE, subject)?;
            Ok(file)
        }

        pub(super) fn try_acquire_writer_lock_windows(
            queue_root: &Path,
        ) -> StorageResult<Option<WriterLockGuard>> {
            let _dir_pins = pin_queue_root_chain(queue_root)?;
            let lock_path = writer_lock_path(queue_root);
            let file = open_nofollow_file(
                &lock_path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_ALWAYS,
                "capture queue writer.lock",
                "open capture queue writer.lock",
            )?;
            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            // SAFETY: `file` is valid and `overlapped` is zero-initialized.
            let rc = unsafe {
                LockFileEx(
                    file.as_raw_handle() as HANDLE,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            if rc != 0 {
                return Ok(Some(WriterLockGuard {
                    _file: std::sync::Arc::new(file),
                }));
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                return Ok(None);
            }
            Err(StorageBackendError::io(
                "acquire capture queue writer.lock",
                err,
            ))
        }

        pub(super) fn is_writer_active_windows(queue_root: &Path) -> StorageResult<bool> {
            let _dir_pins = match pin_queue_root_chain(queue_root) {
                Ok(pins) => pins,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => return Ok(false),
                Err(err) => return Err(err),
            };
            let lock_path = writer_lock_path(queue_root);
            let file = match open_nofollow_file(
                &lock_path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue writer.lock",
                "probe capture queue writer.lock",
            ) {
                Ok(f) => f,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => return Ok(false),
                Err(err) => return Err(err),
            };
            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            // SAFETY: `file` is valid and `overlapped` is zero-initialized.
            let rc = unsafe {
                LockFileEx(
                    file.as_raw_handle() as HANDLE,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            if rc != 0 {
                return Ok(false);
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                return Ok(true);
            }
            Err(StorageBackendError::io(
                "probe capture queue writer.lock",
                err,
            ))
        }

        fn read_meta_replica_windows(
            lane_dir: &Path,
            file_name: &str,
            subject: &'static str,
        ) -> StorageResult<ReplicaRead> {
            let path = lane_dir.join(file_name);
            let file = match open_nofollow_file(
                &path,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                subject,
                "open capture queue metadata",
            ) {
                Ok(f) => f,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => return Ok(ReplicaRead::Absent),
                Err(err) => return Err(err),
            };
            let mut bytes = Vec::with_capacity(LTM2_MAX_LEN + 1);
            file.take((LTM2_MAX_LEN + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| StorageBackendError::io("read capture queue metadata", e))?;
            if bytes.len() > LTM2_MAX_LEN {
                return Ok(ReplicaRead::Corrupt);
            }
            match LaneTailMeta::decode(&bytes) {
                Ok(meta) => Ok(ReplicaRead::Valid(meta)),
                Err(_) => Ok(ReplicaRead::Corrupt),
            }
        }

        pub(super) fn inspect_lane_readonly_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
        ) -> StorageResult<ConsumerLaneSnapshot> {
            // Pin `private/`, `queue_root`, and `lane_dir` (omitting `FILE_SHARE_DELETE`)
            // for the entire duration of replica reads and segment directory scan.
            let _dir_pins = pin_queue_root_chain(queue_root)?;
            let lane_dir = queue_root.join(lane_subdir_name(lane));
            let _lane_pin = pin_dir_not_reparse(&lane_dir, "capture queue lane directory")?;
            let primary = read_meta_replica_windows(
                &lane_dir,
                QUEUE_TAIL_META_FILE,
                "capture queue tail.meta",
            )?;
            let backup = read_meta_replica_windows(
                &lane_dir,
                QUEUE_TAIL_META_BAK_FILE,
                "capture queue tail.meta.bak",
            )?;
            let mut resolved = resolve_consumer_replicas(primary, backup)?;

            let mut seg_ids = Vec::new();
            for entry in fs::read_dir(&lane_dir)
                .map_err(|e| StorageBackendError::io("read capture queue lane directory", e))?
            {
                let entry = entry
                    .map_err(|e| StorageBackendError::io("read capture queue lane entry", e))?;
                let raw_name = entry.file_name();
                if let Some(name) = raw_name.to_str() {
                    if matches_segment_log_glob_bytes(name.as_bytes()) {
                        match parse_segment_log_file_name(name.as_bytes()) {
                            Some(seg_id) => seg_ids.push(seg_id),
                            None => {
                                return Err(StorageBackendError::CorruptCommittedFrame {
                                    detail:
                                        "malformed events-*.log segment filename in capture queue lane"
                                            .to_string(),
                                });
                            }
                        }
                    }
                } else {
                    let units: Vec<u16> = raw_name.encode_wide().collect();
                    let prefix: [u16; 7] = [
                        b'e' as u16,
                        b'v' as u16,
                        b'e' as u16,
                        b'n' as u16,
                        b't' as u16,
                        b's' as u16,
                        b'-' as u16,
                    ];
                    let suffix: [u16; 4] = [b'.' as u16, b'l' as u16, b'o' as u16, b'g' as u16];
                    if units.len() >= prefix.len() + suffix.len()
                        && units.starts_with(&prefix)
                        && units.ends_with(&suffix)
                    {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail: "malformed events-*.log segment filename in capture queue lane"
                                .to_string(),
                        });
                    }
                }
            }
            seg_ids.sort_unstable();
            seg_ids.dedup();

            if resolved.is_none() && !seg_ids.is_empty() {
                let primary_retry = read_meta_replica_windows(
                    &lane_dir,
                    QUEUE_TAIL_META_FILE,
                    "capture queue tail.meta",
                )?;
                let backup_retry = read_meta_replica_windows(
                    &lane_dir,
                    QUEUE_TAIL_META_BAK_FILE,
                    "capture queue tail.meta.bak",
                )?;
                resolved = resolve_consumer_replicas(primary_retry, backup_retry)?;
            }

            if seg_ids.is_empty() {
                return match resolved {
                    None => Ok(ConsumerLaneSnapshot::Empty),
                    Some(meta) if meta.is_initial_empty() => Ok(ConsumerLaneSnapshot::Empty),
                    Some(_) => Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "Case 0c: zero segment files present while LTM2 records non-empty committed state"
                            .to_string(),
                    }),
                };
            }

            let Some(meta) = resolved else {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "Subcase B2: segment files exist while both LTM2 tail.meta replicas are absent"
                        .to_string(),
                });
            };
            Ok(ConsumerLaneSnapshot::Populated { meta, seg_ids })
        }

        pub(super) fn open_lane_segment_readonly_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
            seg_id: u32,
        ) -> StorageResult<(File, u64)> {
            // Keep `private/`, `queue_root`, and `lane_dir` pinned across
            // segment open and stat so no parent directory can be swapped.
            let _dir_pins = pin_queue_root_chain(queue_root)?;
            let lane_dir = queue_root.join(lane_subdir_name(lane));
            let _lane_pin = pin_dir_not_reparse(&lane_dir, "capture queue lane directory")?;
            let seg_path = lane_dir.join(segment_log_file_name(seg_id));
            let file = match open_nofollow_file(
                &seg_path,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                "capture queue segment file",
                "open capture queue segment",
            ) {
                Ok(f) => f,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "capture queue segment file is missing".to_string(),
                    });
                }
                Err(err) => return Err(err),
            };
            let meta = file
                .metadata()
                .map_err(|e| StorageBackendError::io("stat capture queue segment", e))?;
            Ok((file, meta.len()))
        }
    }

    pub(super) fn try_acquire_writer_lock_non_unix(
        queue_root: &Path,
    ) -> StorageResult<Option<WriterLockGuard>> {
        #[cfg(windows)]
        {
            win_writer::try_acquire_writer_lock_windows(queue_root)
        }
        #[cfg(not(windows))]
        {
            let _ = queue_root;
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue writer.lock is unsupported on this platform".to_string(),
            })
        }
    }

    pub(super) fn is_writer_active_non_unix(queue_root: &Path) -> StorageResult<bool> {
        #[cfg(windows)]
        {
            win_writer::is_writer_active_windows(queue_root)
        }
        #[cfg(not(windows))]
        {
            let _ = queue_root;
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue writer.lock probe is unsupported on this platform"
                    .to_string(),
            })
        }
    }

    pub(super) fn inspect_lane_readonly_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
    ) -> StorageResult<ConsumerLaneSnapshot> {
        #[cfg(windows)]
        {
            win_writer::inspect_lane_readonly_windows(queue_root, lane)
        }
        #[cfg(not(windows))]
        {
            let _ = (queue_root, lane);
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue consumer is unsupported on this platform".to_string(),
            })
        }
    }

    pub(super) fn open_lane_segment_readonly_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
    ) -> StorageResult<(File, u64)> {
        #[cfg(windows)]
        {
            win_writer::open_lane_segment_readonly_windows(queue_root, lane, seg_id)
        }
        #[cfg(not(windows))]
        {
            let _ = (queue_root, lane, seg_id);
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue consumer is unsupported on this platform".to_string(),
            })
        }
    }
}

// ============================================================================
// Tests (`RB-09` AC1–AC5)
// ============================================================================

#[cfg(all(test, feature = "storage-redb"))]
mod tests {
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::storage::{
        backend::StorageBackendKind,
        capture_store::{
            AgentAuditLogRecord, AgentBridgeCheckpointRecord, AgentBridgeEventRecord,
            AgentBridgeLinkRecord, AgentBridgeOperationRecord, AgentBridgeSessionRecord,
            AgentBridgeStore, AgentCaptureCloudBaseRecord, AgentCaptureIncarnationRecord,
            AgentCheckpointPruneTombstoneRecord, AgentCheckpointRecord, AgentCoverageClaimRecord,
            AgentCoverageConflictRecord, AgentCoverageRevisionRecord, AgentExportJobRecord,
            AgentImportIdentityRecord, AgentImportTombstoneRecord, AgentSessionCommitRecord,
            AgentSessionRecord, AgentSubagentContentClaimRecord,
            AgentSubagentContentRevisionRecord, AgentSubagentLinkRecord, AgentUsageStatsRecord,
            AgentWorkspaceScopeAuditRecord, BridgeEventAppendInput, BridgeEventAppendOutcome,
            BridgeLinkWrite, BridgeSessionOpenInput, BridgeSessionState, CaptureAgentKind,
            CaptureBatchOutcome, CaptureCatalogCopyReceiptRecord, CaptureSchemaVersionRecord,
            CaptureScopeState, CaptureSessionState, RedbCaptureStore,
        },
        queue::{
            CaptureQueueOccupancy, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
            QUARANTINE_DIAGNOSTIC_CODE, QUEUE_TAIL_META_BAK_TMP_FILE, QUEUE_TAIL_META_TMP_FILE,
            QueueAppendOptions, SealedSegmentMeta, append_postcard_frame, crc32c_checksum,
            encode_postcard_frame,
            highwater::{highwater_mirror_path, read_highwater_mirror},
            init_capture_queue, read_converged_lane_tail_meta,
            segment::{
                append_encoded_frame_in_storage_dir_with_observer, reclaim_sealed_segment_at_root,
            },
            tail_meta_bak_path, tail_meta_path,
        },
    };

    const TEST_AUDIT_CHECKPOINT_ID: &str = "cp-rb09-normal";

    #[derive(Debug, Clone)]
    struct RecordedBatchCall {
        lane: CaptureQueueLane,
        item_count: usize,
        applied_count: usize,
        deduplicated_count: usize,
        dedup_keys: Vec<String>,
        highwater: CaptureQueueHighwater,
        db_highwater_after: CaptureQueueHighwater,
        tx_delta: u64,
        append_lock_was_free: bool,
    }

    type AfterBatchCallback = Arc<dyn Fn(&RecordedBatchCall) + Send + Sync>;

    /// Non-blocking OS-level probe returning `true` iff `capture-queue/append.lock`
    /// is currently uncontended (or absent), supported on BOTH Unix (`flock`) and
    /// Windows (`LockFileEx`).
    fn probe_append_lock_is_free(queue_root: &Path) -> bool {
        let lock_path = queue_root.join(crate::storage::queue::QUEUE_APPEND_LOCK_FILE);
        if !lock_path.exists() {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd as _;
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open append.lock for non-blocking probe");
            // SAFETY: `file` is valid.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            rc == 0
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::{
                Foundation::{ERROR_LOCK_VIOLATION, HANDLE},
                Storage::FileSystem::{
                    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
                },
                System::IO::OVERLAPPED,
            };
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open append.lock for Windows non-blocking probe");
            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            // SAFETY: `file` is valid and `overlapped` is zero-initialized.
            let rc = unsafe {
                LockFileEx(
                    file.as_raw_handle() as HANDLE,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            if rc != 0 {
                true
            } else {
                let err = io::Error::last_os_error();
                assert_eq!(
                    err.raw_os_error(),
                    Some(ERROR_LOCK_VIOLATION as i32),
                    "unexpected Windows LockFileEx probe error: {err:?}"
                );
                false
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            true
        }
    }

    /// Spy wrapper delegating every `CaptureStore` / `AgentBridgeStore` call to
    /// a real `RedbCaptureStore` while recording exact `append_capture_batch`
    /// call order and asserting `writer.lock` IS held while `append.lock` is
    /// NEVER held by the consumer across DB writes (`GC-RB-04`).
    struct SpyCaptureStore {
        inner: RedbCaptureStore,
        queue_root: PathBuf,
        calls: Arc<Mutex<Vec<RecordedBatchCall>>>,
        consumer_holds_append_lock: Option<Arc<std::sync::atomic::AtomicBool>>,
        external_append_lock_held: Option<Arc<std::sync::atomic::AtomicBool>>,
        on_after_batch: Option<AfterBatchCallback>,
    }

    impl SpyCaptureStore {
        fn new(inner: RedbCaptureStore, queue_root: PathBuf) -> Self {
            Self {
                inner,
                queue_root,
                calls: Arc::new(Mutex::new(Vec::new())),
                consumer_holds_append_lock: None,
                external_append_lock_held: None,
                on_after_batch: None,
            }
        }

        fn with_after_batch_callback(
            mut self,
            callback: impl Fn(&RecordedBatchCall) + Send + Sync + 'static,
        ) -> Self {
            self.on_after_batch = Some(Arc::new(callback));
            self
        }

        fn new_with_lock_tracking(
            inner: RedbCaptureStore,
            queue_root: PathBuf,
            consumer_holds_append_lock: Arc<std::sync::atomic::AtomicBool>,
            external_append_lock_held: Arc<std::sync::atomic::AtomicBool>,
        ) -> Self {
            Self {
                consumer_holds_append_lock: Some(consumer_holds_append_lock),
                external_append_lock_held: Some(external_append_lock_held),
                ..Self::new(inner, queue_root)
            }
        }

        fn recorded_calls(&self) -> Vec<RecordedBatchCall> {
            self.calls.lock().expect("lock recorded calls").clone()
        }

        /// Assert `writer.lock` IS held and `capture-queue/append.lock` is NOT
        /// held by the consumer while executing `CaptureStore::append_capture_batch`.
        /// Returns whether the OS `append.lock` file was uncontended during the DB write.
        fn assert_lock_invariants_during_db_write(&self) -> bool {
            assert!(
                is_writer_active_at_root(&self.queue_root)
                    .expect("probe writer.lock during DB write"),
                "consumer must hold capture-queue/writer.lock during CaptureStore::append_capture_batch"
            );
            if let Some(ref consumer_held) = self.consumer_holds_append_lock {
                assert!(
                    !consumer_held.load(std::sync::atomic::Ordering::Acquire),
                    "consumer must NEVER hold capture-queue/append.lock during CaptureStore::append_capture_batch"
                );
            }
            let append_lock_free = probe_append_lock_is_free(&self.queue_root);
            if !append_lock_free {
                let external_allowed = self
                    .external_append_lock_held
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire));
                assert!(
                    external_allowed,
                    "consumer must NEVER hold capture-queue/append.lock during CaptureStore::append_capture_batch (no active external producer lock holder)"
                );
            }
            append_lock_free
        }
    }

    #[async_trait]
    impl AgentBridgeStore for SpyCaptureStore {
        async fn open_bridge_session(
            &self,
            input: &BridgeSessionOpenInput,
        ) -> StorageResult<(AgentBridgeSessionRecord, bool)> {
            self.inner.open_bridge_session(input).await
        }
        async fn get_bridge_session(
            &self,
            bridge_session_id: &str,
        ) -> StorageResult<Option<AgentBridgeSessionRecord>> {
            self.inner.get_bridge_session(bridge_session_id).await
        }
        async fn set_bridge_session_state(
            &self,
            bridge_session_id: &str,
            state: BridgeSessionState,
            updated_at: i64,
        ) -> StorageResult<()> {
            self.inner
                .set_bridge_session_state(bridge_session_id, state, updated_at)
                .await
        }
        async fn delete_bridge_session(&self, bridge_session_id: &str) -> StorageResult<bool> {
            self.inner.delete_bridge_session(bridge_session_id).await
        }
        async fn append_and_ack_events(
            &self,
            bridge_session_id: &str,
            events: &[BridgeEventAppendInput],
            now_ms: i64,
        ) -> StorageResult<BridgeEventAppendOutcome> {
            self.inner
                .append_and_ack_events(bridge_session_id, events, now_ms)
                .await
        }
        async fn get_bridge_event_by_seq(
            &self,
            bridge_session_id: &str,
            event_seq: i64,
        ) -> StorageResult<Option<AgentBridgeEventRecord>> {
            self.inner
                .get_bridge_event_by_seq(bridge_session_id, event_seq)
                .await
        }
        async fn list_bridge_events(
            &self,
            bridge_session_id: &str,
        ) -> StorageResult<Vec<AgentBridgeEventRecord>> {
            self.inner.list_bridge_events(bridge_session_id).await
        }
        async fn list_bridge_events_by_time(&self) -> StorageResult<Vec<AgentBridgeEventRecord>> {
            self.inner.list_bridge_events_by_time().await
        }
        async fn put_bridge_checkpoint(
            &self,
            record: &AgentBridgeCheckpointRecord,
        ) -> StorageResult<()> {
            self.inner.put_bridge_checkpoint(record).await
        }
        async fn get_bridge_checkpoint(
            &self,
            checkpoint_id: &str,
        ) -> StorageResult<Option<AgentBridgeCheckpointRecord>> {
            self.inner.get_bridge_checkpoint(checkpoint_id).await
        }
        async fn list_bridge_checkpoints(
            &self,
            bridge_session_id: &str,
        ) -> StorageResult<Vec<AgentBridgeCheckpointRecord>> {
            self.inner.list_bridge_checkpoints(bridge_session_id).await
        }
        async fn put_bridge_operation(
            &self,
            record: &AgentBridgeOperationRecord,
        ) -> StorageResult<()> {
            self.inner.put_bridge_operation(record).await
        }
        async fn get_bridge_operation(
            &self,
            operation_id: &str,
        ) -> StorageResult<Option<AgentBridgeOperationRecord>> {
            self.inner.get_bridge_operation(operation_id).await
        }
        async fn list_bridge_operations(
            &self,
            bridge_session_id: &str,
        ) -> StorageResult<Vec<AgentBridgeOperationRecord>> {
            self.inner.list_bridge_operations(bridge_session_id).await
        }
        async fn insert_bridge_link(
            &self,
            link: &BridgeLinkWrite,
        ) -> StorageResult<(AgentBridgeLinkRecord, bool)> {
            self.inner.insert_bridge_link(link).await
        }
        async fn list_bridge_links(
            &self,
            bridge_session_id: &str,
        ) -> StorageResult<Vec<AgentBridgeLinkRecord>> {
            self.inner.list_bridge_links(bridge_session_id).await
        }
    }

    #[async_trait]
    impl CaptureStore for SpyCaptureStore {
        fn backend_kind(&self) -> StorageBackendKind {
            self.inner.backend_kind()
        }

        fn is_read_only(&self) -> bool {
            self.inner.is_read_only()
        }

        async fn append_capture_batch(
            &self,
            items: &[CaptureBatchItem],
            highwater_advance: Option<(CaptureQueueLane, CaptureQueueHighwater)>,
        ) -> StorageResult<CaptureBatchOutcome> {
            let append_lock_was_free = self.assert_lock_invariants_during_db_write();
            let tx_before = self.inner.write_transaction_count();
            let outcome = self
                .inner
                .append_capture_batch(items, highwater_advance)
                .await?;
            let tx_delta = self.inner.write_transaction_count() - tx_before;
            if let Some((lane, hw)) = highwater_advance {
                let db_highwater_after = self.inner.get_queue_highwater(lane).await?;
                let recorded = RecordedBatchCall {
                    lane,
                    item_count: items.len(),
                    applied_count: outcome.applied_count,
                    deduplicated_count: outcome.deduplicated_count,
                    dedup_keys: items.iter().map(|i| i.dedup_key.clone()).collect(),
                    highwater: outcome.highwater.unwrap_or(hw),
                    db_highwater_after,
                    tx_delta,
                    append_lock_was_free,
                };
                if let Some(ref cb) = self.on_after_batch {
                    cb(&recorded);
                }
                self.calls.lock().expect("lock calls").push(recorded);
            }
            Ok(outcome)
        }

        async fn get_queue_highwater(
            &self,
            lane: CaptureQueueLane,
        ) -> StorageResult<CaptureQueueHighwater> {
            self.inner.get_queue_highwater(lane).await
        }

        async fn put_schema_version(
            &self,
            record: &CaptureSchemaVersionRecord,
        ) -> StorageResult<()> {
            self.inner.put_schema_version(record).await
        }
        async fn list_schema_versions(&self) -> StorageResult<Vec<CaptureSchemaVersionRecord>> {
            self.inner.list_schema_versions().await
        }
        async fn put_copy_receipt(
            &self,
            record: &CaptureCatalogCopyReceiptRecord,
        ) -> StorageResult<()> {
            self.inner.put_copy_receipt(record).await
        }
        async fn get_copy_receipt(&self) -> StorageResult<Option<CaptureCatalogCopyReceiptRecord>> {
            self.inner.get_copy_receipt().await
        }
        async fn append_audit_log(&self, record: &AgentAuditLogRecord) -> StorageResult<()> {
            self.inner.append_audit_log(record).await
        }
        async fn get_audit_log(
            &self,
            audit_id: &str,
        ) -> StorageResult<Option<AgentAuditLogRecord>> {
            self.inner.get_audit_log(audit_id).await
        }
        async fn list_audit_logs_by_checkpoint(
            &self,
            checkpoint_id: &str,
        ) -> StorageResult<Vec<AgentAuditLogRecord>> {
            self.inner
                .list_audit_logs_by_checkpoint(checkpoint_id)
                .await
        }
        async fn put_cloud_base(&self, record: &AgentCaptureCloudBaseRecord) -> StorageResult<()> {
            self.inner.put_cloud_base(record).await
        }
        async fn get_cloud_base(
            &self,
            repo_id: &str,
        ) -> StorageResult<Option<AgentCaptureCloudBaseRecord>> {
            self.inner.get_cloud_base(repo_id).await
        }
        async fn put_incarnation(
            &self,
            record: &AgentCaptureIncarnationRecord,
        ) -> StorageResult<()> {
            self.inner.put_incarnation(record).await
        }
        async fn get_incarnation(
            &self,
            agent_kind: &str,
            provider_session_id: &str,
        ) -> StorageResult<Option<AgentCaptureIncarnationRecord>> {
            self.inner
                .get_incarnation(agent_kind, provider_session_id)
                .await
        }
        async fn upsert_session(&self, record: &AgentSessionRecord) -> StorageResult<()> {
            self.inner.upsert_session(record).await
        }
        async fn get_session(&self, session_id: &str) -> StorageResult<Option<AgentSessionRecord>> {
            self.inner.get_session(session_id).await
        }
        async fn list_sessions_by_started_desc(&self) -> StorageResult<Vec<AgentSessionRecord>> {
            self.inner.list_sessions_by_started_desc().await
        }
        async fn delete_session(&self, session_id: &str) -> StorageResult<bool> {
            self.inner.delete_session(session_id).await
        }
        async fn upsert_checkpoint(&self, record: &AgentCheckpointRecord) -> StorageResult<()> {
            self.inner.upsert_checkpoint(record).await
        }
        async fn get_checkpoint(
            &self,
            checkpoint_id: &str,
        ) -> StorageResult<Option<AgentCheckpointRecord>> {
            self.inner.get_checkpoint(checkpoint_id).await
        }
        async fn list_checkpoints_by_session(
            &self,
            session_id: &str,
        ) -> StorageResult<Vec<AgentCheckpointRecord>> {
            self.inner.list_checkpoints_by_session(session_id).await
        }
        async fn delete_checkpoint(&self, checkpoint_id: &str) -> StorageResult<bool> {
            self.inner.delete_checkpoint(checkpoint_id).await
        }
        async fn put_checkpoint_prune_tombstone(
            &self,
            record: &AgentCheckpointPruneTombstoneRecord,
        ) -> StorageResult<()> {
            self.inner.put_checkpoint_prune_tombstone(record).await
        }
        async fn get_checkpoint_prune_tombstone(
            &self,
            checkpoint_id: &str,
        ) -> StorageResult<Option<AgentCheckpointPruneTombstoneRecord>> {
            self.inner
                .get_checkpoint_prune_tombstone(checkpoint_id)
                .await
        }
        async fn upsert_coverage_claim(
            &self,
            record: &AgentCoverageClaimRecord,
        ) -> StorageResult<()> {
            self.inner.upsert_coverage_claim(record).await
        }
        async fn get_coverage_claim(
            &self,
            session_id: &str,
            logical_turn_key: &str,
            coverage_schema_version: i64,
        ) -> StorageResult<Option<AgentCoverageClaimRecord>> {
            self.inner
                .get_coverage_claim(session_id, logical_turn_key, coverage_schema_version)
                .await
        }
        async fn upsert_coverage_conflict(
            &self,
            record: &AgentCoverageConflictRecord,
        ) -> StorageResult<()> {
            self.inner.upsert_coverage_conflict(record).await
        }
        async fn get_coverage_conflict(
            &self,
            session_id: &str,
            logical_turn_key: &str,
            coverage_schema_version: i64,
        ) -> StorageResult<Option<AgentCoverageConflictRecord>> {
            self.inner
                .get_coverage_conflict(session_id, logical_turn_key, coverage_schema_version)
                .await
        }
        async fn put_coverage_revision(
            &self,
            record: &AgentCoverageRevisionRecord,
        ) -> StorageResult<()> {
            self.inner.put_coverage_revision(record).await
        }
        async fn list_coverage_revisions(
            &self,
            session_id: &str,
            logical_turn_key: &str,
            coverage_schema_version: i64,
        ) -> StorageResult<Vec<AgentCoverageRevisionRecord>> {
            self.inner
                .list_coverage_revisions(session_id, logical_turn_key, coverage_schema_version)
                .await
        }
        async fn upsert_export_job(&self, record: &AgentExportJobRecord) -> StorageResult<()> {
            self.inner.upsert_export_job(record).await
        }
        async fn get_export_job(
            &self,
            job_id: &str,
        ) -> StorageResult<Option<AgentExportJobRecord>> {
            self.inner.get_export_job(job_id).await
        }
        async fn upsert_import_identity(
            &self,
            record: &AgentImportIdentityRecord,
        ) -> StorageResult<()> {
            self.inner.upsert_import_identity(record).await
        }
        async fn get_import_identity(
            &self,
            identity_id: &str,
        ) -> StorageResult<Option<AgentImportIdentityRecord>> {
            self.inner.get_import_identity(identity_id).await
        }
        async fn put_import_tombstone(
            &self,
            record: &AgentImportTombstoneRecord,
        ) -> StorageResult<()> {
            self.inner.put_import_tombstone(record).await
        }
        async fn get_import_tombstone(
            &self,
            tombstone_id: &str,
        ) -> StorageResult<Option<AgentImportTombstoneRecord>> {
            self.inner.get_import_tombstone(tombstone_id).await
        }
        async fn list_import_tombstones(&self) -> StorageResult<Vec<AgentImportTombstoneRecord>> {
            self.inner.list_import_tombstones().await
        }
        async fn put_session_commit(&self, record: &AgentSessionCommitRecord) -> StorageResult<()> {
            self.inner.put_session_commit(record).await
        }
        async fn list_session_commits(
            &self,
            session_id: &str,
        ) -> StorageResult<Vec<AgentSessionCommitRecord>> {
            self.inner.list_session_commits(session_id).await
        }
        async fn upsert_subagent_content_claim(
            &self,
            record: &AgentSubagentContentClaimRecord,
        ) -> StorageResult<()> {
            self.inner.upsert_subagent_content_claim(record).await
        }
        async fn get_subagent_content_claim(
            &self,
            parent_session_id: &str,
            provider_kind: &str,
            source_key: &str,
            content_schema_version: i64,
        ) -> StorageResult<Option<AgentSubagentContentClaimRecord>> {
            self.inner
                .get_subagent_content_claim(
                    parent_session_id,
                    provider_kind,
                    source_key,
                    content_schema_version,
                )
                .await
        }
        async fn put_subagent_content_revision(
            &self,
            record: &AgentSubagentContentRevisionRecord,
        ) -> StorageResult<()> {
            self.inner.put_subagent_content_revision(record).await
        }
        async fn list_subagent_content_revisions(
            &self,
            parent_session_id: &str,
            provider_kind: &str,
            source_key: &str,
            content_schema_version: i64,
        ) -> StorageResult<Vec<AgentSubagentContentRevisionRecord>> {
            self.inner
                .list_subagent_content_revisions(
                    parent_session_id,
                    provider_kind,
                    source_key,
                    content_schema_version,
                )
                .await
        }
        async fn upsert_subagent_link(
            &self,
            record: &AgentSubagentLinkRecord,
        ) -> StorageResult<()> {
            self.inner.upsert_subagent_link(record).await
        }
        async fn get_subagent_link(
            &self,
            content_checkpoint_id: &str,
        ) -> StorageResult<Option<AgentSubagentLinkRecord>> {
            self.inner.get_subagent_link(content_checkpoint_id).await
        }
        async fn put_usage_stats(&self, record: &AgentUsageStatsRecord) -> StorageResult<()> {
            self.inner.put_usage_stats(record).await
        }
        async fn get_usage_stats(&self, id: &str) -> StorageResult<Option<AgentUsageStatsRecord>> {
            self.inner.get_usage_stats(id).await
        }
        async fn list_usage_stats_by_session(
            &self,
            session_id: &str,
        ) -> StorageResult<Vec<AgentUsageStatsRecord>> {
            self.inner.list_usage_stats_by_session(session_id).await
        }
        async fn append_workspace_scope_audit(
            &self,
            record: &AgentWorkspaceScopeAuditRecord,
        ) -> StorageResult<()> {
            self.inner.append_workspace_scope_audit(record).await
        }
        async fn get_workspace_scope_audit(
            &self,
            audit_id: &str,
        ) -> StorageResult<Option<AgentWorkspaceScopeAuditRecord>> {
            self.inner.get_workspace_scope_audit(audit_id).await
        }
        async fn list_workspace_scope_audits(
            &self,
            agent_kind: &str,
            provider_session_id: &str,
        ) -> StorageResult<Vec<AgentWorkspaceScopeAuditRecord>> {
            self.inner
                .list_workspace_scope_audits(agent_kind, provider_session_id)
                .await
        }
    }

    fn make_session_payload(
        session_id: &str,
        state: CaptureSessionState,
        started_at: i64,
    ) -> CaptureBatchPayload {
        CaptureBatchPayload::UpsertSession(AgentSessionRecord {
            session_id: session_id.to_string(),
            agent_kind: CaptureAgentKind::ClaudeCode,
            provider_session_id: format!("prov-{session_id}"),
            thread_id: Some("thread-rb09".to_string()),
            state,
            working_dir: "/workspace".to_string(),
            worktree_id: Some("wt-main".to_string()),
            parent_commit: Some("a".repeat(40)),
            parent_session_id: None,
            metadata_json: r#"{"event":"SessionEnd"}"#.to_string(),
            redaction_report: r#"{"redacted":0}"#.to_string(),
            started_at,
            last_event_at: started_at + 100,
            stopped_at: if state == CaptureSessionState::Stopped {
                Some(started_at + 100)
            } else {
                None
            },
            schema_version: 1,
            sync_revision: 1,
            repo_id: Some("repo-rb09".to_string()),
            workspace_id: Some("ws-rb09".to_string()),
            workspace_fence: Some(1),
            scope_state: CaptureScopeState::Scoped,
        })
    }

    fn make_audit_payload(audit_id: &str, timestamp: &str) -> CaptureBatchPayload {
        CaptureBatchPayload::AppendAuditLog(AgentAuditLogRecord {
            audit_id: audit_id.to_string(),
            timestamp: timestamp.to_string(),
            user_id: Some("tester@libra.local".to_string()),
            user_name: Some("Tester".to_string()),
            action: "capture_event".to_string(),
            checkpoint_id: TEST_AUDIT_CHECKPOINT_ID.to_string(),
            scope: "normal".to_string(),
            export_path: None,
            justification: Some("rb09 queue drain".to_string()),
            granted: true,
        })
    }

    fn write_bytes_0600(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write test fixture file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod 0600");
        }
    }

    #[tokio::test]
    async fn priority_lane_drains_before_normal_with_contiguous_highwater() {
        // `RB-09` AC1 & AC2:
        // - AC1: With 200 normal events backlogged in `normal/` and 1 `SessionEnd`
        //   frame in `priority/`, the writer strictly commits `priority/` FIRST
        //   before consuming ANY batch from `normal/`.
        // - AC2: Both `priority/` and `normal/` advance strictly monotonically
        //   and contiguously with zero gaps, and external mirror agrees with DB.
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let capture_db_path = repo
            .path()
            .join(PRIVATE_SUBDIR)
            .join("capture.gen-0001.redb");
        let redb_store = RedbCaptureStore::create(&capture_db_path)
            .await
            .expect("create RedbCaptureStore");
        let spy = SpyCaptureStore::new(redb_store.clone(), init.paths.root.clone());

        // Backlog 200 normal events in `normal/`.
        let mut expected_normal_end = 0u64;
        for idx in 0..200u32 {
            let dedup = format!("normal:audit:{idx:04}");
            let payload = make_audit_payload(
                &format!("audit-{idx:04}"),
                &format!("2026-11-23T12:00:{:02}Z", idx % 60),
            );
            let out = append_postcard_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload,
            )
            .expect("enqueue normal event");
            assert_eq!(out.frame_offset, expected_normal_end);
            expected_normal_end += out.frame_len as u64;
        }

        // Enqueue 1 `SessionEnd` frame in `priority/`.
        let session_end_payload = make_session_payload(
            "sess-priority-end-1",
            CaptureSessionState::Stopped,
            1_700_000_000,
        );
        let p_out = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "priority:session-end:sess-priority-end-1",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &session_end_payload,
        )
        .expect("enqueue priority SessionEnd");

        let report = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("drain capture queue");
        assert!(report.acquired_writer_lock);
        assert_eq!(report.priority_batches, 1);
        assert_eq!(
            report.normal_batches, 2,
            "200 normal events => 128 + 72 = 2 batches"
        );
        assert_eq!(report.applied_frames, 201);
        assert_eq!(report.deduplicated_frames, 0);

        let calls = spy.recorded_calls();
        assert_eq!(calls.len(), 3);

        // AC1: `priority/` MUST be committed strictly BEFORE ANY `normal/` batch.
        assert_eq!(
            calls[0].lane,
            CaptureQueueLane::Priority,
            "AC1 violated: first DB batch must drain priority/ before normal/"
        );
        assert_eq!(calls[0].item_count, 1);
        assert_eq!(
            calls[0].dedup_keys,
            vec!["priority:session-end:sess-priority-end-1".to_string()]
        );
        assert_eq!(calls[1].lane, CaptureQueueLane::Normal);
        assert_eq!(calls[1].item_count, 128);
        assert_eq!(calls[2].lane, CaptureQueueLane::Normal);
        assert_eq!(calls[2].item_count, 72);

        // AC2: strictly monotonic contiguous highwaters per lane with zero gaps.
        assert_eq!(
            calls[0].highwater,
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: p_out.meta.committed_len,
            }
        );
        assert!(calls[1].highwater.byte_offset > 0);
        assert!(calls[1].highwater.byte_offset < expected_normal_end);
        assert_eq!(calls[1].highwater.segment_id, 0);
        assert_eq!(
            calls[2].highwater,
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: expected_normal_end,
            }
        );

        let db_prio_hw = redb_store
            .get_queue_highwater(CaptureQueueLane::Priority)
            .await
            .unwrap();
        let db_norm_hw = redb_store
            .get_queue_highwater(CaptureQueueLane::Normal)
            .await
            .unwrap();
        assert_eq!(
            db_prio_hw,
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: p_out.meta.committed_len,
            }
        );
        assert_eq!(
            db_norm_hw,
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: expected_normal_end,
            }
        );

        // External advisory mirror agrees with authoritative DB highwater.
        let mirror = read_highwater_mirror(&init.paths.root)
            .expect("read highwater mirror")
            .expect("highwater mirror must exist");
        assert_eq!(mirror.priority, db_prio_hw);
        assert_eq!(mirror.normal, db_norm_hw);

        // All 201 business rows persisted cleanly in capture.redb.
        let ended_sess = redb_store
            .get_session("sess-priority-end-1")
            .await
            .unwrap()
            .expect("priority SessionEnd must be persisted");
        assert_eq!(ended_sess.state, CaptureSessionState::Stopped);
        let audits = redb_store
            .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
            .await
            .unwrap();
        assert_eq!(audits.len(), 200);
    }

    #[tokio::test]
    async fn batch_128_single_tx_dedup_and_segment_gc() {
        // `RB-09` AC3, AC4 & AC5:
        // - AC3: 128 consecutive `normal/` events coalesce into 1 SINGLE
        //   `CaptureStore::append_capture_batch` redb `WriteTransaction` and
        //   atomically advance `QUEUE_HIGHWATER_NORMAL` inside that same tx.
        // - AC4: Replaying a frame with a duplicate full semantic `dedup_key`
        //   is idempotent and leaves exactly ONE business record in capture.redb.
        // - AC5: Consumer acquires `append.lock` in `writer.lock -> append.lock`
        //   order, verifies `seg == min_live_seg && seg < active_seg && seg <
        //   highwater_seg`, atomically updates dual `LTM2` (`MIN_LIVE_SEG = 1`,
        //   removing `sealed[0]`) and unlinks + `fsync_dir` historical `seg 0`.
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let capture_db_path = repo
            .path()
            .join(PRIVATE_SUBDIR)
            .join("capture.gen-0001.redb");
        let redb_store = RedbCaptureStore::create(&capture_db_path)
            .await
            .expect("create RedbCaptureStore");
        let spy = SpyCaptureStore::new(redb_store.clone(), init.paths.root.clone());

        // =====================================================================
        // Part 1 — AC3: Exactly 128 events -> 1 SINGLE redb WriteTransaction.
        // =====================================================================
        let mut expected_end = 0u64;
        for idx in 0..128u32 {
            let dedup = format!("rb09:batch128:audit:{idx:04}");
            let payload = make_audit_payload(
                &format!("batch128-audit-{idx:04}"),
                &format!("2026-11-23T13:00:{:02}Z", idx % 60),
            );
            let out = append_postcard_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload,
            )
            .expect("enqueue batch128 event");
            expected_end += out.frame_len as u64;
        }

        let tx_before = redb_store.write_transaction_count();
        let report_128 = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("drain 128 normal events");
        let tx_after = redb_store.write_transaction_count();

        assert!(report_128.acquired_writer_lock);
        assert_eq!(report_128.normal_batches, 1);
        assert_eq!(report_128.applied_frames, 128);
        assert_eq!(report_128.deduplicated_frames, 0);
        assert_eq!(
            tx_after - tx_before,
            1,
            "AC3 violated: 128 normal events must commit in 1 SINGLE redb WriteTransaction"
        );
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: expected_end,
            }
        );
        assert_eq!(
            redb_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            128
        );

        // =====================================================================
        // Part 1b — AC3 (50ms arriving-frame window): 1 frame initially present
        // + 127 frames committed incrementally during the 50ms collection
        // window coalesce into 1 SINGLE redb WriteTransaction with contiguous
        // highwater, and a priority SessionEnd arriving mid-window immediately
        // preempts the wait.
        // =====================================================================
        {
            let win_repo = tempdir().expect("tempdir");
            let win_init = init_capture_queue(win_repo.path()).expect("init window queue");
            let win_db_path = win_repo
                .path()
                .join(PRIVATE_SUBDIR)
                .join("capture.gen-0001.redb");
            let win_store = RedbCaptureStore::create(&win_db_path)
                .await
                .expect("create window RedbCaptureStore");
            let win_spy = SpyCaptureStore::new(win_store.clone(), win_init.paths.root.clone());

            // Pre-encode all 128 frames so the hook commits frames 1..128 in
            // two arrivals within the 50ms collection window (< 2ms wall time).
            let mut frames_bytes: Vec<Vec<u8>> = Vec::with_capacity(128);
            for idx in 0..128u32 {
                let raw = encode_postcard_frame(
                    FRAME_FLAG_NONE,
                    &format!("rb09:window:audit:{idx:04}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload(
                        &format!("win-audit-{idx:04}"),
                        &format!("2026-11-23T13:30:{:02}Z", idx % 60),
                    ),
                )
                .expect("encode window frame");
                frames_bytes.push(raw);
            }

            // Seed only frame 0 before starting the drain.
            let win_normal_dir = win_init.paths.normal.clone();
            let seg0_path = win_normal_dir.join(segment_log_file_name(0));
            let mut seg0_buf = frames_bytes[0].clone();
            write_bytes_0600(&seg0_path, &seg0_buf);
            let init_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: seg0_buf.len() as u64,
                prefix_crc32c: crc32c_checksum(&seg0_buf),
                sealed: Vec::new(),
            };
            let raw_init_meta = init_meta.encode().expect("encode init window LTM2");
            write_bytes_0600(&tail_meta_path(&win_normal_dir), &raw_init_meta);
            write_bytes_0600(&tail_meta_bak_path(&win_normal_dir), &raw_init_meta);

            let mut poll_step = 0usize;
            let hook_dir = win_normal_dir.clone();
            let hook_seg0 = seg0_path.clone();
            let mut hook_buf = seg0_buf.clone();
            let rest_frames = frames_bytes[1..].to_vec();
            let sim_clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let hook_clock = Arc::clone(&sim_clock);
            let mut observer = WriterObserver::with_window_poll_hook_and_clock(
                move |collected| {
                    if poll_step == 0 {
                        assert_eq!(collected, 1);
                        hook_clock.store(10, std::sync::atomic::Ordering::Release);
                        for f in &rest_frames[..63] {
                            hook_buf.extend_from_slice(f);
                        }
                    } else if poll_step == 1 {
                        assert_eq!(collected, 64);
                        hook_clock.store(25, std::sync::atomic::Ordering::Release);
                        for f in &rest_frames[63..] {
                            hook_buf.extend_from_slice(f);
                        }
                    } else {
                        return;
                    }
                    poll_step += 1;
                    write_bytes_0600(&hook_seg0, &hook_buf);
                    let updated = LaneTailMeta {
                        min_live_seg: 0,
                        active_seg: 0,
                        committed_len: hook_buf.len() as u64,
                        prefix_crc32c: crc32c_checksum(&hook_buf),
                        sealed: Vec::new(),
                    };
                    let enc = updated.encode().expect("encode updated window LTM2");
                    write_bytes_0600(&tail_meta_path(&hook_dir), &enc);
                    write_bytes_0600(&tail_meta_bak_path(&hook_dir), &enc);
                },
                sim_clock,
            );

            for f in &frames_bytes[1..] {
                seg0_buf.extend_from_slice(f);
            }
            let expected_win_end = seg0_buf.len() as u64;

            let lock_guard = try_acquire_writer_lock_at_root(&win_init.paths.root)
                .expect("acquire writer.lock")
                .expect("writer.lock uncontended");
            let win_tx_before = win_store.write_transaction_count();
            let win_report = drain_locked_at_root(
                &win_init.paths.root,
                &win_spy,
                QueueWriterOptions::default(),
                &lock_guard,
                &mut observer,
            )
            .await
            .expect("drain arriving frames inside 50ms window");
            let win_tx_after = win_store.write_transaction_count();
            drop(lock_guard);

            assert_eq!(win_report.normal_batches, 1);
            assert_eq!(win_report.applied_frames, 128);
            assert_eq!(
                win_tx_after - win_tx_before,
                1,
                "AC3 violated: arriving frames collected within 50ms window must coalesce into 1 SINGLE WriteTransaction"
            );
            assert_eq!(
                win_store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: expected_win_end,
                }
            );
            assert_eq!(
                win_store
                    .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                    .await
                    .unwrap()
                    .len(),
                128
            );

            // Verify priority arrival mid-window preempts the normal batch
            // BEFORE committing any normal DB transaction (`RB-09` AC1):
            // exact DB tx order must be `[Priority, Normal]`, and `normal/`
            // must yield immediately on the first poll without burning the
            // 50ms window.
            let _ = append_postcard_frame(
                win_repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb09:window:preempt-normal",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("win-preempt-normal", "2026-11-23T13:31:00Z"),
            )
            .expect("enqueue single normal frame for preemption check");

            let preempt_spy = SpyCaptureStore::new(win_store.clone(), win_init.paths.root.clone());
            let prio_dir = win_init.paths.priority.clone();
            let prio_clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let prio_hook_clock = Arc::clone(&prio_clock);
            let poll_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hook_polls = Arc::clone(&poll_count);
            let mut prio_observer = WriterObserver::with_window_poll_hook_and_clock(
                move |_collected| {
                    let prev = hook_polls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    if prev == 0 {
                        // First poll: inject priority SessionEnd while simulated
                        // clock is still at 5ms (well inside 50ms window).
                        prio_hook_clock.store(5, std::sync::atomic::Ordering::Release);
                        let p_frame = encode_postcard_frame(
                            FRAME_FLAG_PRIORITY,
                            "rb09:window:preempt-priority",
                            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                            &make_session_payload(
                                "sess-window-preempt",
                                CaptureSessionState::Stopped,
                                1_700_000_500,
                            ),
                        )
                        .expect("encode preempting priority frame");
                        write_bytes_0600(&prio_dir.join(segment_log_file_name(0)), &p_frame);
                        let p_meta = LaneTailMeta {
                            min_live_seg: 0,
                            active_seg: 0,
                            committed_len: p_frame.len() as u64,
                            prefix_crc32c: crc32c_checksum(&p_frame),
                            sealed: Vec::new(),
                        };
                        let enc = p_meta.encode().expect("encode preempting priority LTM2");
                        write_bytes_0600(&tail_meta_path(&prio_dir), &enc);
                        write_bytes_0600(&tail_meta_bak_path(&prio_dir), &enc);
                    } else {
                        // After priority has been committed and normal re-enters,
                        // advance simulated clock past 50ms so normal commits cleanly.
                        prio_hook_clock.store(50, std::sync::atomic::Ordering::Release);
                    }
                },
                prio_clock,
            );
            let lock_guard = try_acquire_writer_lock_at_root(&win_init.paths.root)
                .expect("acquire writer.lock")
                .expect("writer.lock uncontended");
            let preempt_report = drain_locked_at_root(
                &win_init.paths.root,
                &preempt_spy,
                QueueWriterOptions::default(),
                &lock_guard,
                &mut prio_observer,
            )
            .await
            .expect("drain with priority preemption");
            drop(lock_guard);
            assert_eq!(preempt_report.priority_batches, 1);
            assert_eq!(preempt_report.normal_batches, 1);
            assert_eq!(
                poll_count.load(std::sync::atomic::Ordering::Acquire),
                2,
                "normal must yield immediately on poll 1 for priority, then commit on poll 2"
            );
            let preempt_calls = preempt_spy.recorded_calls();
            assert_eq!(preempt_calls.len(), 2);
            assert_eq!(
                preempt_calls[0].lane,
                CaptureQueueLane::Priority,
                "AC1 violated: preempting priority SessionEnd must commit strictly BEFORE normal batch"
            );
            assert_eq!(
                preempt_calls[1].lane,
                CaptureQueueLane::Normal,
                "normal batch must commit strictly AFTER preempting priority batch"
            );
            assert!(
                win_store
                    .get_session("sess-window-preempt")
                    .await
                    .unwrap()
                    .is_some(),
                "preempting priority SessionEnd must be committed immediately"
            );
        }

        // =====================================================================
        // Part 2 — AC4: Duplicate full semantic `dedup_key` replay is idempotent.
        // =====================================================================
        let dup_payload = make_audit_payload(
            "batch128-audit-dup-should-not-insert",
            "2026-11-23T13:01:00Z",
        );
        let dup_out = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb09:batch128:audit:0042",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &dup_payload,
        )
        .expect("enqueue duplicate dedup_key frame");
        let report_dup = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("drain duplicate frame");
        assert_eq!(report_dup.applied_frames, 0);
        assert_eq!(report_dup.deduplicated_frames, 1);
        assert_eq!(
            redb_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            128,
            "AC4 violated: duplicate dedup_key must produce only ONE business record"
        );
        assert!(
            redb_store
                .get_audit_log("batch128-audit-dup-should-not-insert")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: dup_out.meta.committed_len,
            }
        );

        // =====================================================================
        // Part 3 — AC5: Rotated historical segment GC under
        // `writer.lock -> capture-queue/append.lock` (LTM2 updated BEFORE unlink).
        // =====================================================================
        let gc_repo = tempdir().expect("tempdir");
        let gc_init = init_capture_queue(gc_repo.path()).expect("init gc capture queue");
        let gc_db_path = gc_repo
            .path()
            .join(PRIVATE_SUBDIR)
            .join("capture.gen-0001.redb");
        let gc_store = RedbCaptureStore::create(&gc_db_path)
            .await
            .expect("create gc RedbCaptureStore");
        let gc_spy = SpyCaptureStore::new(gc_store.clone(), gc_init.paths.root.clone());
        let normal_dir = &gc_init.paths.normal;

        // Build rotated multi-segment queue: sealed seg 0 (3 frames) + active seg 1 (2 frames).
        let mut s0_bytes = Vec::new();
        for i in 0..3u32 {
            let f = encode_postcard_frame(
                FRAME_FLAG_NONE,
                &format!("gc:s0:{i}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload(&format!("gc-s0-{i}"), "2026-11-23T14:00:00Z"),
            )
            .expect("encode s0 frame");
            s0_bytes.extend_from_slice(&f);
        }
        let s0_crc = crc32c_checksum(&s0_bytes);
        let seg0_path = normal_dir.join(segment_log_file_name(0));
        write_bytes_0600(&seg0_path, &s0_bytes);

        let mut s1_bytes = Vec::new();
        for i in 0..2u32 {
            let f = encode_postcard_frame(
                FRAME_FLAG_NONE,
                &format!("gc:s1:{i}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload(&format!("gc-s1-{i}"), "2026-11-23T14:01:00Z"),
            )
            .expect("encode s1 frame");
            s1_bytes.extend_from_slice(&f);
        }
        let s1_crc = crc32c_checksum(&s1_bytes);
        let seg1_path = normal_dir.join(segment_log_file_name(1));
        write_bytes_0600(&seg1_path, &s1_bytes);

        let rotated_meta = LaneTailMeta {
            min_live_seg: 0,
            active_seg: 1,
            committed_len: s1_bytes.len() as u64,
            prefix_crc32c: s1_crc,
            sealed: vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: s0_bytes.len() as u64,
                sealed_crc32c: s0_crc,
            }],
        };
        let raw_rotated = rotated_meta.encode().expect("encode rotated LTM2");
        write_bytes_0600(&tail_meta_path(normal_dir), &raw_rotated);
        write_bytes_0600(&tail_meta_bak_path(normal_dir), &raw_rotated);

        assert!(seg0_path.exists());
        assert!(seg1_path.exists());

        // Also exercise priority preemption during sealed `normal/` segment
        // collection: a `priority/` SessionEnd arriving while sealed `seg 0`
        // is decoded MUST commit strictly before the sealed `normal/` batch!
        let gc_prio_dir = gc_init.paths.priority.clone();
        let mut injected_sealed_priority = false;
        let mut gc_observer = WriterObserver::with_sealed_pre_commit_hook(move |seg_id| {
            if !injected_sealed_priority && seg_id == 0 {
                injected_sealed_priority = true;
                let p_frame = encode_postcard_frame(
                    FRAME_FLAG_PRIORITY,
                    "gc:priority:preempt-sealed",
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_session_payload(
                        "sess-gc-sealed-preempt",
                        CaptureSessionState::Stopped,
                        1_700_000_900,
                    ),
                )
                .expect("encode sealed-preempting priority frame");
                write_bytes_0600(&gc_prio_dir.join(segment_log_file_name(0)), &p_frame);
                let p_meta = LaneTailMeta {
                    min_live_seg: 0,
                    active_seg: 0,
                    committed_len: p_frame.len() as u64,
                    prefix_crc32c: crc32c_checksum(&p_frame),
                    sealed: Vec::new(),
                };
                let enc = p_meta.encode().expect("encode sealed-preempting LTM2");
                write_bytes_0600(&tail_meta_path(&gc_prio_dir), &enc);
                write_bytes_0600(&tail_meta_bak_path(&gc_prio_dir), &enc);
            }
        });
        let gc_lock = try_acquire_writer_lock_at_root(&gc_init.paths.root)
            .expect("acquire gc writer.lock")
            .expect("gc writer.lock uncontended");
        let gc_report = drain_locked_at_root(
            &gc_init.paths.root,
            &gc_spy,
            QueueWriterOptions::default(),
            &gc_lock,
            &mut gc_observer,
        )
        .await
        .expect("drain rotated queue with priority preemption and reclaim seg 0");
        drop(gc_lock);

        assert!(gc_report.acquired_writer_lock);
        assert_eq!(gc_report.priority_batches, 1);
        assert_eq!(gc_report.normal_batches, 2);
        assert_eq!(gc_report.applied_frames, 6);
        assert_eq!(gc_report.reclaimed_segments, 1);
        let gc_calls = gc_spy.recorded_calls();
        assert_eq!(gc_calls.len(), 3);
        assert_eq!(
            gc_calls[0].lane,
            CaptureQueueLane::Priority,
            "AC1 violated: priority arrival during sealed normal segment read must commit strictly BEFORE sealed normal batch"
        );
        assert_eq!(gc_calls[1].lane, CaptureQueueLane::Normal);
        assert_eq!(gc_calls[1].item_count, 3);
        assert_eq!(gc_calls[2].lane, CaptureQueueLane::Normal);
        assert_eq!(gc_calls[2].item_count, 2);
        assert!(
            gc_store
                .get_session("sess-gc-sealed-preempt")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            gc_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            5
        );

        // AC5: seg 0 was unlinked, seg 1 remains active, dual LTM2 has
        // `min_live_seg == 1` and `sealed.is_empty()`.
        assert!(
            !seg0_path.exists(),
            "AC5 violated: consumed sealed segment 0 must be unlinked"
        );
        assert!(
            seg1_path.exists(),
            "AC5 violated: active segment 1 must remain on disk"
        );
        let post_meta = read_converged_lane_tail_meta(normal_dir).expect("read post-gc LTM2");
        assert_eq!(post_meta.min_live_seg, 1);
        assert_eq!(post_meta.active_seg, 1);
        assert!(post_meta.sealed.is_empty());
        assert_eq!(
            fs::read(tail_meta_path(normal_dir)).unwrap(),
            fs::read(tail_meta_bak_path(normal_dir)).unwrap()
        );
        assert_eq!(
            gc_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater {
                segment_id: 1,
                byte_offset: s1_bytes.len() as u64,
            }
        );

        // Subsequent producer append to seg 1 works seamlessly after reclamation!
        let f_post = append_postcard_frame(
            gc_repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "gc:s1:post",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &make_audit_payload("gc-s1-post", "2026-11-23T14:02:00Z"),
        )
        .expect("append to active seg 1 after seg 0 reclamation must succeed");
        assert_eq!(f_post.seg_id, 1);
        let post_report = drain_capture_queue(gc_repo.path(), &gc_spy)
            .await
            .expect("drain post-gc frame");
        assert_eq!(post_report.applied_frames, 1);
        assert_eq!(
            gc_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            6
        );
    }

    /// Recursively snapshot EVERY regular file (relative path -> raw bytes)
    /// under `queue_root`, including `writer.lock`, `highwater`, `priority/`,
    /// `normal/`, and `quarantine/`, with zero exclusions (`RB-09b`).
    fn snapshot_queue_tree_full(queue_root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(base)
                    .expect("strip queue_root prefix")
                    .to_string_lossy()
                    .replace('\\', "/");
                let meta = fs::symlink_metadata(&path).expect("symlink_metadata");
                if meta.is_dir() {
                    walk(base, &path, out);
                } else if meta.is_file() {
                    out.insert(rel, fs::read(&path).expect("read snapshot file"));
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(queue_root, queue_root, &mut out);
        out
    }

    /// Bootstrap `writer.lock` once before injecting a failure fixture so the
    /// full-tree snapshot includes `writer.lock` verbatim with zero exclusions.
    fn bootstrap_writer_lock(queue_root: &Path) {
        let guard = try_acquire_writer_lock_at_root(queue_root)
            .expect("bootstrap writer.lock")
            .expect("writer.lock uncontended");
        drop(guard);
    }

    fn assert_corrupt_committed_frame(err: StorageBackendError, ctx: &str) {
        assert!(
            matches!(err, StorageBackendError::CorruptCommittedFrame { .. }),
            "{ctx}: expected CorruptCommittedFrame, got {err:?}"
        );
        assert_eq!(
            err.stable_code_str(),
            "LBR-IO-001",
            "{ctx}: CorruptCommittedFrame must map to stable code LBR-IO-001"
        );
    }

    #[tokio::test]
    async fn crash_after_commit_before_mirror_zero_dup() {
        // `RB-09b` AC1:
        // Fault injection: `CaptureStore::append_capture_batch` commits
        // successfully to `capture.redb` (advancing DB `QUEUE_HIGHWATER_*`),
        // then consumer crashes BEFORE updating external `<queue_root>/highwater`
        // mirror (`AfterDbCommitBeforeMirror`). Restart MUST recover strictly
        // from authoritative DB `QUEUE_HIGHWATER_*` with ZERO duplicate records,
        // ignoring absent, stale-behind, corrupt, or stale-ahead external mirrors,
        // and self-healing the external mirror on both 0-new-frame and new-frame restarts.
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let capture_db_path = repo
            .path()
            .join(PRIVATE_SUBDIR)
            .join("capture.gen-0001.redb");
        let redb_store = RedbCaptureStore::create(&capture_db_path)
            .await
            .expect("create RedbCaptureStore");
        let spy = SpyCaptureStore::new(redb_store.clone(), init.paths.root.clone());

        // Enqueue 3 normal frames + 1 priority frame.
        let mut norm_off3 = 0u64;
        for idx in 0..3u32 {
            let out = append_postcard_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &format!("rb09b:ac1:normal:{idx}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload(&format!("ac1-norm-{idx}"), "2026-11-23T15:00:00Z"),
            )
            .expect("enqueue normal frame");
            norm_off3 = out.meta.committed_len;
        }
        let prio_out = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb09b:ac1:priority:0",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &make_session_payload(
                "ac1-sess-prio-0",
                CaptureSessionState::Stopped,
                1_700_001_000,
            ),
        )
        .expect("enqueue priority frame");
        let prio_off1 = prio_out.meta.committed_len;

        // Crash 1 (priority lane commit succeeds, crash BEFORE external mirror exists).
        let lock = try_acquire_writer_lock_at_root(&init.paths.root)
            .expect("acquire writer.lock")
            .expect("writer.lock free");
        let mut crash_obs =
            WriterObserver::with_crash_point(WriterCrashPoint::AfterDbCommitBeforeMirror);
        let crash_err = drain_locked_at_root(
            &init.paths.root,
            &spy,
            QueueWriterOptions::default(),
            &lock,
            &mut crash_obs,
        )
        .await
        .expect_err("must inject crash right after priority DB commit before mirror update");
        assert!(
            matches!(crash_err, StorageBackendError::Io { .. }),
            "expected injected Io crash, got {crash_err:?}"
        );
        drop(lock);

        // Authoritative DB committed priority, external mirror still absent.
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Priority)
                .await
                .unwrap(),
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: prio_off1,
            }
        );
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater::default()
        );
        assert!(
            read_highwater_mirror(&init.paths.root)
                .expect("read absent mirror")
                .is_none(),
            "mirror must still be absent after crash before mirror write"
        );
        assert!(
            redb_store
                .get_session("ac1-sess-prio-0")
                .await
                .unwrap()
                .is_some()
        );

        // Crash 2 (restart resumes from DB priority highwater -> skips already-committed
        // priority frame with ZERO re-read/dup, commits 3 normal frames to DB, then
        // crashes BEFORE updating mirror).
        let lock = try_acquire_writer_lock_at_root(&init.paths.root)
            .expect("acquire writer.lock")
            .expect("writer.lock free");
        let mut crash_obs2 =
            WriterObserver::with_crash_point(WriterCrashPoint::AfterDbCommitBeforeMirror);
        let _ = drain_locked_at_root(
            &init.paths.root,
            &spy,
            QueueWriterOptions::default(),
            &lock,
            &mut crash_obs2,
        )
        .await
        .expect_err("must inject crash right after normal DB commit before mirror update");
        drop(lock);

        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: norm_off3,
            }
        );
        assert!(
            read_highwater_mirror(&init.paths.root)
                .expect("read mirror after normal crash")
                .is_none()
        );
        assert_eq!(
            redb_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            3
        );

        // Restart 1 with ZERO new frames -> must self-heal absent mirror from DB
        // with ZERO DB writes and ZERO duplicates!
        let tx_before_zero_frame_restart = redb_store.write_transaction_count();
        let report_heal_absent = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("restart after mirror crash with 0 new frames");
        assert!(report_heal_absent.acquired_writer_lock);
        assert_eq!(report_heal_absent.priority_batches, 0);
        assert_eq!(report_heal_absent.normal_batches, 0);
        assert_eq!(report_heal_absent.applied_frames, 0);
        assert_eq!(report_heal_absent.deduplicated_frames, 0);
        assert_eq!(
            redb_store.write_transaction_count(),
            tx_before_zero_frame_restart,
            "0-new-frame restart must NOT replay committed frames"
        );
        let mirror_healed = read_highwater_mirror(&init.paths.root)
            .expect("read healed mirror")
            .expect("mirror must be healed from authoritative DB");
        assert_eq!(mirror_healed.priority.byte_offset, prio_off1);
        assert_eq!(mirror_healed.normal.byte_offset, norm_off3);

        // Now enqueue 2 more normal frames (3..5) and crash AGAIN before mirror update
        // so mirror is STALE-BEHIND DB (`norm_off3` vs `norm_off5`)!
        let mut norm_off5 = norm_off3;
        for idx in 3..5u32 {
            let out = append_postcard_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &format!("rb09b:ac1:normal:{idx}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload(&format!("ac1-norm-{idx}"), "2026-11-23T15:01:00Z"),
            )
            .expect("enqueue normal frame 3..5");
            norm_off5 = out.meta.committed_len;
        }
        let lock = try_acquire_writer_lock_at_root(&init.paths.root)
            .expect("acquire writer.lock")
            .expect("writer.lock free");
        let mut crash_obs3 =
            WriterObserver::with_crash_point(WriterCrashPoint::AfterDbCommitBeforeMirror);
        let _ = drain_locked_at_root(
            &init.paths.root,
            &spy,
            QueueWriterOptions::default(),
            &lock,
            &mut crash_obs3,
        )
        .await
        .expect_err("crash after normal batch 2 before mirror update");
        drop(lock);

        // Mirror is stale behind DB (`norm_off3` < `norm_off5`).
        assert_eq!(
            read_highwater_mirror(&init.paths.root)
                .unwrap()
                .unwrap()
                .normal
                .byte_offset,
            norm_off3
        );
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap()
                .byte_offset,
            norm_off5
        );

        // Enqueue frame 5 and restart: must consume ONLY frame 5 (zero replay of 3..5),
        // zero duplicates, and advance mirror to `norm_off6`.
        let out6 = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb09b:ac1:normal:5",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &make_audit_payload("ac1-norm-5", "2026-11-23T15:02:00Z"),
        )
        .expect("enqueue normal frame 5");
        let norm_off6 = out6.meta.committed_len;

        let report_stale = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("restart from stale-behind mirror");
        assert_eq!(report_stale.applied_frames, 1);
        assert_eq!(report_stale.deduplicated_frames, 0);
        assert_eq!(
            redb_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            read_highwater_mirror(&init.paths.root)
                .unwrap()
                .unwrap()
                .normal
                .byte_offset,
            norm_off6
        );

        // Corrupt mirror bytes (bad CRC / truncated garbage) AND stale-AHEAD mirror
        // (claiming higher offset than authoritative DB) -> DB MUST win in both cases!
        let mirror_path = crate::storage::queue::highwater_mirror_path(&init.paths.root);
        write_bytes_0600(&mirror_path, b"CORRUPT_MIRROR_BYTES");
        assert!(read_highwater_mirror(&init.paths.root).unwrap().is_none());
        let report_corrupt_mirror = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("restart with corrupt external mirror must recover from DB");
        assert_eq!(report_corrupt_mirror.applied_frames, 0);
        assert_eq!(
            read_highwater_mirror(&init.paths.root)
                .unwrap()
                .unwrap()
                .normal
                .byte_offset,
            norm_off6
        );

        // Stale-AHEAD mirror: enqueue frame 6, tamper mirror to claim `norm_off7`
        // BEFORE DB commits frame 6 -> consumer MUST trust DB (`norm_off6`), NOT skip frame 6!
        let out7 = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb09b:ac1:normal:6",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &make_audit_payload("ac1-norm-6", "2026-11-23T15:03:00Z"),
        )
        .expect("enqueue normal frame 6");
        let norm_off7 = out7.meta.committed_len;
        let fake_ahead = QueueHighwaterSnapshot {
            priority: CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: prio_off1,
            },
            normal: CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: norm_off7,
            },
        };
        write_highwater_mirror(&init.paths.root, &fake_ahead).expect("plant ahead mirror");
        let report_ahead = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("restart with stale-ahead mirror must obey authoritative DB");
        assert_eq!(
            report_ahead.applied_frames, 1,
            "AC1 violated: stale-ahead external mirror must NEVER cause consumer to skip uncommitted frames"
        );
        assert_eq!(report_ahead.deduplicated_frames, 0);
        assert_eq!(
            redb_store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            7
        );
        assert_eq!(
            redb_store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap()
                .byte_offset,
            norm_off7
        );

        // =====================================================================
        // Full parameterized AC1 restart matrix across BOTH lanes (`Priority` &
        // `Normal`) × ALL 4 mirror states (`Absent`, `StaleBehind`, `Corrupt`,
        // `StaleAhead`) × BOTH 0-new-frame and new-frame restarts!
        // =====================================================================
        #[derive(Clone, Copy)]
        enum MirrorFault {
            Absent,
            StaleBehind,
            Corrupt,
            StaleAhead,
        }
        for lane in [CaptureQueueLane::Priority, CaptureQueueLane::Normal] {
            for fault in [
                MirrorFault::Absent,
                MirrorFault::StaleBehind,
                MirrorFault::Corrupt,
                MirrorFault::StaleAhead,
            ] {
                for restart_with_new_frame in [false, true] {
                    let m_repo = tempdir().expect("tempdir");
                    let m_init = init_capture_queue(m_repo.path()).expect("init matrix queue");
                    let m_store = RedbCaptureStore::create(
                        m_repo
                            .path()
                            .join(PRIVATE_SUBDIR)
                            .join("capture.gen-0001.redb"),
                    )
                    .await
                    .expect("create matrix RedbCaptureStore");
                    let flags = if lane == CaptureQueueLane::Priority {
                        FRAME_FLAG_PRIORITY
                    } else {
                        FRAME_FLAG_NONE
                    };
                    let f0 = append_postcard_frame(
                        m_repo.path(),
                        lane,
                        flags,
                        &format!("ac1:mat:{}:f0", lane.as_str()),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &make_audit_payload("ac1-mat-0", "2026-11-23T15:10:00Z"),
                    )
                    .unwrap();
                    let off1 = f0.meta.committed_len;

                    // For `StaleBehind`, establish mirror at `off1` first, then commit `f1`
                    // and crash BEFORE mirror update.
                    let committed_off = match fault {
                        MirrorFault::StaleBehind => {
                            let rep0 = drain_capture_queue(m_repo.path(), &m_store).await.unwrap();
                            assert_eq!(rep0.applied_frames, 1);
                            let f1 = append_postcard_frame(
                                m_repo.path(),
                                lane,
                                flags,
                                &format!("ac1:mat:{}:f1", lane.as_str()),
                                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                                &make_audit_payload("ac1-mat-1", "2026-11-23T15:10:01Z"),
                            )
                            .unwrap();
                            let lock = try_acquire_writer_lock_at_root(&m_init.paths.root)
                                .unwrap()
                                .unwrap();
                            let mut obs = WriterObserver::with_crash_point(
                                WriterCrashPoint::AfterDbCommitBeforeMirror,
                            );
                            let _ = drain_locked_at_root(
                                &m_init.paths.root,
                                &m_store,
                                QueueWriterOptions::default(),
                                &lock,
                                &mut obs,
                            )
                            .await
                            .expect_err("inject stale-behind mirror crash");
                            drop(lock);
                            assert_eq!(
                                read_highwater_mirror(&m_init.paths.root)
                                    .unwrap()
                                    .unwrap()
                                    .get(lane)
                                    .byte_offset,
                                off1
                            );
                            f1.meta.committed_len
                        }
                        MirrorFault::Absent | MirrorFault::Corrupt | MirrorFault::StaleAhead => {
                            let lock = try_acquire_writer_lock_at_root(&m_init.paths.root)
                                .unwrap()
                                .unwrap();
                            let mut obs = WriterObserver::with_crash_point(
                                WriterCrashPoint::AfterDbCommitBeforeMirror,
                            );
                            let _ = drain_locked_at_root(
                                &m_init.paths.root,
                                &m_store,
                                QueueWriterOptions::default(),
                                &lock,
                                &mut obs,
                            )
                            .await
                            .expect_err("inject mirror crash");
                            drop(lock);
                            if matches!(fault, MirrorFault::Corrupt) {
                                let mp = crate::storage::queue::highwater_mirror_path(
                                    &m_init.paths.root,
                                );
                                write_bytes_0600(&mp, b"BAD_HWM1_CRC");
                            }
                            off1
                        }
                    };
                    let base_count = if matches!(fault, MirrorFault::StaleBehind) {
                        2
                    } else {
                        1
                    };

                    let mut expected_off = committed_off;
                    let mut expected_applied = 0usize;
                    if restart_with_new_frame {
                        let f_new = append_postcard_frame(
                            m_repo.path(),
                            lane,
                            flags,
                            &format!("ac1:mat:{}:fnew", lane.as_str()),
                            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                            &make_audit_payload("ac1-mat-new", "2026-11-23T15:10:02Z"),
                        )
                        .unwrap();
                        expected_off = f_new.meta.committed_len;
                        expected_applied = 1;
                    }
                    if matches!(fault, MirrorFault::StaleAhead) {
                        let mut ahead_snap = QueueHighwaterSnapshot::default();
                        ahead_snap.set(
                            lane,
                            CaptureQueueHighwater {
                                segment_id: 0,
                                byte_offset: expected_off + 256,
                            },
                        );
                        write_highwater_mirror(&m_init.paths.root, &ahead_snap).unwrap();
                    }

                    let tx_before = m_store.write_transaction_count();
                    let rep = drain_capture_queue(m_repo.path(), &m_store)
                        .await
                        .expect("matrix restart must recover cleanly from authoritative DB");
                    assert!(rep.acquired_writer_lock);
                    assert_eq!(rep.applied_frames, expected_applied);
                    assert_eq!(rep.deduplicated_frames, 0);
                    assert_eq!(
                        m_store.write_transaction_count() - tx_before,
                        if restart_with_new_frame { 1 } else { 0 }
                    );
                    assert_eq!(
                        m_store.get_queue_highwater(lane).await.unwrap(),
                        CaptureQueueHighwater {
                            segment_id: 0,
                            byte_offset: expected_off,
                        }
                    );
                    assert_eq!(
                        read_highwater_mirror(&m_init.paths.root)
                            .unwrap()
                            .expect("mirror must be healed on restart")
                            .get(lane),
                        CaptureQueueHighwater {
                            segment_id: 0,
                            byte_offset: expected_off,
                        }
                    );
                    assert_eq!(
                        m_store
                            .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                            .await
                            .unwrap()
                            .len(),
                        base_count + expected_applied
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression() {
        // `RB-09b` AC2, AC3, AC4 & AC5:
        // Every fault class MUST fail closed with `CorruptCommittedFrame`
        // (`LBR-IO-001`), leave DB `QUEUE_HIGHWATER_*` & `write_transaction_count()`
        // strictly unchanged, and leave the entire queue tree (`priority/`,
        // `normal/`, `quarantine/`, `highwater`) 100% byte-identical (zero
        // quarantine, zero truncation, zero highwater advance).

        // =====================================================================
        // AC2: Active segment committed prefix (`offset < committed_len`)
        // corruption and truncation rejection (zero quarantine, zero advance).
        // =====================================================================
        {
            // Subcase 2a: bitflip inside `[0, committed_len)` before drain (`hw == 0`).
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init");
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store");
            let mut f_end = 0u64;
            for i in 0..3u32 {
                let out = append_postcard_frame(
                    repo.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    &format!("ac2a:{i}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload(&format!("ac2a-audit-{i}"), "2026-11-23T16:00:00Z"),
                )
                .unwrap();
                f_end = out.meta.committed_len;
            }
            let seg0 = init.paths.normal.join(segment_log_file_name(0));
            let orig_bytes = fs::read(&seg0).unwrap();
            let mut flipped = orig_bytes.clone();
            flipped[20] ^= 0x80;
            write_bytes_0600(&seg0, &flipped);
            bootstrap_writer_lock(&init.paths.root);

            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC2a: committed prefix bitflip must fail closed");
            assert_corrupt_committed_frame(err, "AC2a committed prefix bitflip");
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                CaptureQueueHighwater::default()
            );
            assert_eq!(
                snapshot_queue_tree_full(&init.paths.root),
                tree_before,
                "AC2a must not quarantine, truncate, or modify any queue file"
            );

            // Subcase 2b: restore valid bytes, drain frame 0..3 (`hw == committed_len`),
            // then flip byte in ALREADY-CONSUMED prefix (`offset < hw == committed_len`)
            // AND append an uncommitted half-frame tail -> MUST fail closed BEFORE
            // any tail recovery/quarantine!
            write_bytes_0600(&seg0, &orig_bytes);
            let ok_rep = drain_capture_queue(repo.path(), &store).await.unwrap();
            assert_eq!(ok_rep.applied_frames, 3);
            let hw_committed = store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap();
            assert_eq!(hw_committed.byte_offset, f_end);

            let mut flipped_with_tail = orig_bytes.clone();
            flipped_with_tail[4] ^= 0x01; // flip inside committed prefix
            flipped_with_tail.extend_from_slice(b"LCE1\x10\x00\x00\x00HALF"); // uncommitted tail
            write_bytes_0600(&seg0, &flipped_with_tail);

            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store).await.expect_err(
                "AC2b: bitflip in consumed prefix with uncommitted tail must fail closed",
            );
            assert_corrupt_committed_frame(err, "AC2b consumed prefix bitflip + tail");
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                hw_committed
            );
            assert_eq!(
                snapshot_queue_tree_full(&init.paths.root),
                tree_before,
                "AC2b must NOT quarantine tail when committed prefix is corrupt"
            );

            // Subcase 2c: short active segment (`actual_len < committed_len`):
            // both half-frame truncation AND exact complete-frame `CleanEof` truncation!
            let frame0_len = encode_postcard_frame(
                FRAME_FLAG_NONE,
                "ac2a:0",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2a-audit-0", "2026-11-23T16:00:00Z"),
            )
            .unwrap()
            .len();
            for (label, trunc_len) in [
                ("half-frame short active segment", frame0_len - 7),
                ("complete-frame CleanEof short active segment", frame0_len),
            ] {
                write_bytes_0600(&seg0, &orig_bytes[..trunc_len]);
                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let err = drain_capture_queue(repo.path(), &store)
                    .await
                    .expect_err(label);
                assert_corrupt_committed_frame(err, label);
                assert_eq!(store.write_transaction_count(), tx_before);
                assert_eq!(
                    store
                        .get_queue_highwater(CaptureQueueLane::Normal)
                        .await
                        .unwrap(),
                    hw_committed
                );
                assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);
            }

            // Subcase 2d: corrupt/truncated frame inside `[0, committed_len)` where
            // `LTM2.prefix_crc32c` matches the corrupt bytes (`[valid_frame_0, truncated_frame_1]`):
            // decoder MUST reject BEFORE committing even valid_frame_0!
            let repo_d = tempdir().expect("tempdir");
            let init_d = init_capture_queue(repo_d.path()).expect("init_d");
            let store_d = RedbCaptureStore::create(
                repo_d
                    .path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store_d");
            let f0 = encode_postcard_frame(
                FRAME_FLAG_NONE,
                "ac2d:0",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2d-audit-0", "2026-11-23T16:05:00Z"),
            )
            .unwrap();
            let f1 = encode_postcard_frame(
                FRAME_FLAG_NONE,
                "ac2d:1",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2d-audit-1", "2026-11-23T16:05:01Z"),
            )
            .unwrap();
            let mut corrupt_committed = f0.clone();
            corrupt_committed.extend_from_slice(&f1[..f1.len() - 5]); // half-frame inside committed_len
            let seg0_d = init_d.paths.normal.join(segment_log_file_name(0));
            write_bytes_0600(&seg0_d, &corrupt_committed);
            let bad_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: corrupt_committed.len() as u64,
                prefix_crc32c: crc32c_checksum(&corrupt_committed),
                sealed: Vec::new(),
            };
            let raw_bad = bad_meta.encode().unwrap();
            write_bytes_0600(&tail_meta_path(&init_d.paths.normal), &raw_bad);
            write_bytes_0600(&tail_meta_bak_path(&init_d.paths.normal), &raw_bad);
            bootstrap_writer_lock(&init_d.paths.root);

            let tree_before = snapshot_queue_tree_full(&init_d.paths.root);
            let tx_before = store_d.write_transaction_count();
            let err = drain_capture_queue(repo_d.path(), &store_d)
                .await
                .expect_err("AC2d: truncated frame inside committed_len must fail closed");
            assert_corrupt_committed_frame(err, "AC2d truncated frame inside committed_len");
            assert_eq!(store_d.write_transaction_count(), tx_before);
            assert!(
                store_d
                    .get_audit_log("ac2d-audit-0")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                store_d
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                CaptureQueueHighwater::default()
            );
            assert_eq!(snapshot_queue_tree_full(&init_d.paths.root), tree_before);

            // Subcase 2e: Beyond 128-frame batch limit (`129 valid frames + 1 truncated
            // frame 130` inside `committed_len` with matching `LTM2.prefix_crc32c`):
            // preflight structural scan MUST reject BEFORE committing batch 1!
            let repo_e = tempdir().expect("tempdir");
            let init_e = init_capture_queue(repo_e.path()).expect("init_e");
            let store_e = RedbCaptureStore::create(
                repo_e
                    .path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store_e");
            let mut big_buf = Vec::new();
            for idx in 0..129u32 {
                let raw = encode_postcard_frame(
                    FRAME_FLAG_NONE,
                    &format!("ac2e:{idx:04}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload(&format!("ac2e-audit-{idx:04}"), "2026-11-23T16:06:00Z"),
                )
                .unwrap();
                big_buf.extend_from_slice(&raw);
            }
            let raw_130 = encode_postcard_frame(
                FRAME_FLAG_NONE,
                "ac2e:0129",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2e-audit-0129", "2026-11-23T16:06:01Z"),
            )
            .unwrap();
            big_buf.extend_from_slice(&raw_130[..raw_130.len() - 6]);
            write_bytes_0600(
                &init_e.paths.normal.join(segment_log_file_name(0)),
                &big_buf,
            );
            let big_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: big_buf.len() as u64,
                prefix_crc32c: crc32c_checksum(&big_buf),
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init_e.paths.normal), &big_meta);
            write_bytes_0600(&tail_meta_bak_path(&init_e.paths.normal), &big_meta);
            bootstrap_writer_lock(&init_e.paths.root);

            let tree_before = snapshot_queue_tree_full(&init_e.paths.root);
            let tx_before = store_e.write_transaction_count();
            let err = drain_capture_queue(repo_e.path(), &store_e)
                .await
                .expect_err(
                    "AC2e: malformed frame beyond 128-frame batch limit must fail closed before batch 1",
                );
            assert_corrupt_committed_frame(err, "AC2e malformed frame beyond batch limit");
            assert_eq!(store_e.write_transaction_count(), tx_before);
            assert_eq!(
                store_e
                    .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                    .await
                    .unwrap()
                    .len(),
                0
            );
            assert_eq!(
                store_e
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                CaptureQueueHighwater::default()
            );
            assert_eq!(snapshot_queue_tree_full(&init_e.paths.root), tree_before);

            // Subcase 2f: Malformed frame BEFORE DB highwater (`offset < hw`) with
            // recomputed matching `LTM2.prefix_crc32c` -> MUST fail closed BEFORE
            // committing frame 1!
            let repo_f = tempdir().expect("tempdir");
            let init_f = init_capture_queue(repo_f.path()).expect("init_f");
            let store_f = RedbCaptureStore::create(
                repo_f
                    .path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store_f");
            let f0_out = append_postcard_frame(
                repo_f.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "ac2f:0",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2f-audit-0", "2026-11-23T16:07:00Z"),
            )
            .unwrap();
            let _ = drain_capture_queue(repo_f.path(), &store_f).await.unwrap();
            let hw_f0 = store_f
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap();
            assert_eq!(hw_f0.byte_offset, f0_out.meta.committed_len);
            let _ = append_postcard_frame(
                repo_f.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "ac2f:1",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac2f-audit-1", "2026-11-23T16:07:01Z"),
            )
            .unwrap();
            let seg0_f = init_f.paths.normal.join(segment_log_file_name(0));
            let mut tampered = fs::read(&seg0_f).unwrap();
            tampered[0] ^= 0xFF; // corrupt frame 0 BEFORE DB highwater, then re-sign LTM2
            write_bytes_0600(&seg0_f, &tampered);
            let resigned = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: tampered.len() as u64,
                prefix_crc32c: crc32c_checksum(&tampered),
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init_f.paths.normal), &resigned);
            write_bytes_0600(&tail_meta_bak_path(&init_f.paths.normal), &resigned);

            let tree_before = snapshot_queue_tree_full(&init_f.paths.root);
            let tx_before = store_f.write_transaction_count();
            let err = drain_capture_queue(repo_f.path(), &store_f)
                .await
                .expect_err(
                    "AC2f: structurally corrupt frame before DB highwater must fail closed",
                );
            assert_corrupt_committed_frame(err, "AC2f corrupt frame before DB highwater");
            assert_eq!(store_f.write_transaction_count(), tx_before);
            assert!(
                store_f
                    .get_audit_log("ac2f-audit-1")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                store_f
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                hw_f0
            );
            assert_eq!(snapshot_queue_tree_full(&init_f.paths.root), tree_before);

            // Subcase 2g: Cross-lane preflight atomicity — pending valid `priority/`
            // `SessionEnd` while `normal/` has a corrupt committed prefix (and
            // symmetric: pending valid `normal/` while `priority/` has dual meta loss)
            // MUST fail closed BEFORE committing ANY DB tx or mirror write on EITHER lane!
            for corrupt_normal in [true, false] {
                let repo_g = tempdir().expect("tempdir");
                let init_g = init_capture_queue(repo_g.path()).expect("init_g");
                let store_g = RedbCaptureStore::create(
                    repo_g
                        .path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create store_g");
                let _ = append_postcard_frame(
                    repo_g.path(),
                    CaptureQueueLane::Priority,
                    FRAME_FLAG_PRIORITY,
                    "ac2g:prio",
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_session_payload("ac2g-sess", CaptureSessionState::Stopped, 1_700_003_000),
                )
                .unwrap();
                let _ = append_postcard_frame(
                    repo_g.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    "ac2g:norm",
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload("ac2g-norm", "2026-11-23T16:08:00Z"),
                )
                .unwrap();
                if corrupt_normal {
                    let s = init_g.paths.normal.join(segment_log_file_name(0));
                    let mut b = fs::read(&s).unwrap();
                    b[12] ^= 0x40;
                    write_bytes_0600(&s, &b);
                } else {
                    fs::remove_file(tail_meta_path(&init_g.paths.priority)).unwrap();
                    fs::remove_file(tail_meta_bak_path(&init_g.paths.priority)).unwrap();
                }
                bootstrap_writer_lock(&init_g.paths.root);

                let tree_before = snapshot_queue_tree_full(&init_g.paths.root);
                let tx_before = store_g.write_transaction_count();
                let err = drain_capture_queue(repo_g.path(), &store_g)
                    .await
                    .expect_err(
                        "AC2g: cross-lane corruption must fail closed before either lane commits",
                    );
                assert_corrupt_committed_frame(err, "AC2g cross-lane preflight");
                assert_eq!(
                    store_g.write_transaction_count(),
                    tx_before,
                    "AC2g violated: healthy lane must NOT commit when sibling lane is corrupt"
                );
                assert_eq!(
                    store_g
                        .get_queue_highwater(CaptureQueueLane::Priority)
                        .await
                        .unwrap(),
                    CaptureQueueHighwater::default()
                );
                assert_eq!(
                    store_g
                        .get_queue_highwater(CaptureQueueLane::Normal)
                        .await
                        .unwrap(),
                    CaptureQueueHighwater::default()
                );
                assert!(store_g.get_session("ac2g-sess").await.unwrap().is_none());
                assert!(store_g.get_audit_log("ac2g-norm").await.unwrap().is_none());
                assert_eq!(snapshot_queue_tree_full(&init_g.paths.root), tree_before);
            }
        }

        // =====================================================================
        // AC3: Non-empty (`>= 1` segment) + Subcase B2 dual metadata loss across
        // both `normal/` and `priority/` and all 6 loss/divergence modes.
        // =====================================================================
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            for mode in 0..6u8 {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init");
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create store");
                let flags = if lane == CaptureQueueLane::Priority {
                    FRAME_FLAG_PRIORITY
                } else {
                    FRAME_FLAG_NONE
                };
                let _ = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("ac3:{}:{mode}", lane.as_str()),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload(&format!("ac3-audit-{mode}"), "2026-11-23T16:10:00Z"),
                )
                .unwrap();

                let lane_dir = match lane {
                    CaptureQueueLane::Priority => &init.paths.priority,
                    CaptureQueueLane::Normal => &init.paths.normal,
                };
                let p_path = tail_meta_path(lane_dir);
                let b_path = tail_meta_bak_path(lane_dir);
                let orig_meta = fs::read(&p_path).unwrap();
                let mut bad_crc = orig_meta.clone();
                let last = bad_crc.len() - 1;
                bad_crc[last] ^= 0xFF;

                match mode {
                    0 => {
                        fs::remove_file(&p_path).unwrap();
                        fs::remove_file(&b_path).unwrap();
                    }
                    1 => {
                        write_bytes_0600(&p_path, &bad_crc);
                        write_bytes_0600(&b_path, &bad_crc);
                    }
                    2 => {
                        write_bytes_0600(&p_path, &bad_crc);
                        fs::remove_file(&b_path).unwrap();
                    }
                    3 => {
                        fs::remove_file(&p_path).unwrap();
                        write_bytes_0600(&b_path, &bad_crc);
                    }
                    4 => {
                        write_bytes_0600(&p_path, &orig_meta[..10]); // truncated
                        write_bytes_0600(&b_path, &vec![b'X'; LTM2_MAX_LEN + 16]); // oversized
                    }
                    5 => {
                        // Equal ordering key, divergent prefix_crc32c.
                        let mut decoded = LaneTailMeta::decode(&orig_meta).unwrap();
                        decoded.prefix_crc32c ^= 0x1234_5678;
                        let div = decoded.encode().unwrap();
                        write_bytes_0600(&b_path, &div);
                    }
                    _ => unreachable!(),
                }

                bootstrap_writer_lock(&init.paths.root);
                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let err = drain_capture_queue(repo.path(), &store)
                    .await
                    .expect_err("AC3: dual metadata loss must fail closed");
                assert_corrupt_committed_frame(
                    err,
                    &format!("AC3 lane={} mode={mode}", lane.as_str()),
                );
                assert_eq!(store.write_transaction_count(), tx_before);
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater::default()
                );
                assert_eq!(
                    snapshot_queue_tree_full(&init.paths.root),
                    tree_before,
                    "AC3 must leave queue tree byte-identical on dual metadata loss"
                );
            }
        }

        // =====================================================================
        // AC4: 0 segment files + Case 0c (non-empty committed metadata) OR
        // Case 0d (corrupt/divergent metadata) -> fail-closed zero-mutation.
        // =====================================================================
        for case_idx in 0..8u8 {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init");
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store");
            let lane_dir = &init.paths.normal;
            let p_path = tail_meta_path(lane_dir);
            let b_path = tail_meta_bak_path(lane_dir);

            let nonempty = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: 128,
                prefix_crc32c: 0xDEAD_BEEF,
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            let nonempty_seg1 = LaneTailMeta {
                min_live_seg: 1,
                active_seg: 1,
                committed_len: 0,
                prefix_crc32c: 0,
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            let mut bad_crc = nonempty.clone();
            let last = bad_crc.len() - 1;
            bad_crc[last] ^= 0x55;

            match case_idx {
                0 => {
                    // Case 0c: dual non-empty LTM2 with 0 segments
                    write_bytes_0600(&p_path, &nonempty);
                    write_bytes_0600(&b_path, &nonempty);
                }
                1 => {
                    // Case 0c: single surviving non-empty LTM2 with 0 segments
                    write_bytes_0600(&p_path, &nonempty);
                    fs::remove_file(&b_path).unwrap();
                }
                2 => {
                    // Case 0c: active_seg == 1, committed_len == 0 with 0 segments
                    write_bytes_0600(&p_path, &nonempty_seg1);
                    write_bytes_0600(&b_path, &nonempty_seg1);
                }
                3 => {
                    // Case 0d: both corrupt CRC
                    write_bytes_0600(&p_path, &bad_crc);
                    write_bytes_0600(&b_path, &bad_crc);
                }
                4 => {
                    // Case 0d: primary corrupt + backup absent
                    write_bytes_0600(&p_path, &bad_crc);
                    fs::remove_file(&b_path).unwrap();
                }
                5 => {
                    // Case 0d: primary absent + backup corrupt
                    fs::remove_file(&p_path).unwrap();
                    write_bytes_0600(&b_path, &bad_crc);
                }
                6 => {
                    // Case 0d: oversized + truncated
                    write_bytes_0600(&p_path, &vec![b'Z'; LTM2_MAX_LEN + 4]);
                    write_bytes_0600(&b_path, b"LTM2");
                }
                7 => {
                    // Case 0d: equal ordering key divergent checksum
                    let div = LaneTailMeta {
                        min_live_seg: 0,
                        active_seg: 0,
                        committed_len: 128,
                        prefix_crc32c: 0xCAFE_BABE,
                        sealed: Vec::new(),
                    }
                    .encode()
                    .unwrap();
                    write_bytes_0600(&p_path, &nonempty);
                    write_bytes_0600(&b_path, &div);
                }
                _ => unreachable!(),
            }

            bootstrap_writer_lock(&init.paths.root);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC4: 0-segment non-empty or corrupt meta must fail closed");
            assert_corrupt_committed_frame(err, &format!("AC4 case_idx={case_idx}"));
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                CaptureQueueHighwater::default()
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);
        }

        // =====================================================================
        // AC5: Channel `LTM2` regressed behind authoritative DB `QUEUE_HIGHWATER_*`
        // (same-seg offset, seg-id, reset-to-empty, min_live_seg leapfrog, and
        // rotated `sealed_len < hw.byte_offset`).
        // =====================================================================
        {
            // Subcase 5a: Same-segment offset regression (`DB hw = (0, off_3)`,
            // `LTM2` rolled back to `(0, off_1)`).
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init");
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create store");
            let mut off1 = 0u64;
            let mut off3 = 0u64;
            for i in 0..3u32 {
                let out = append_postcard_frame(
                    repo.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    &format!("ac5a:{i}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &make_audit_payload(&format!("ac5a-audit-{i}"), "2026-11-23T16:20:00Z"),
                )
                .unwrap();
                if i == 0 {
                    off1 = out.meta.committed_len;
                }
                off3 = out.meta.committed_len;
            }
            let _ = drain_capture_queue(repo.path(), &store).await.unwrap();
            let db_hw = store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap();
            assert_eq!(
                db_hw,
                CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: off3
                }
            );

            let seg0 = init.paths.normal.join(segment_log_file_name(0));
            let orig_seg0 = fs::read(&seg0).unwrap();
            let regressed_bytes = &orig_seg0[..off1 as usize];
            write_bytes_0600(&seg0, regressed_bytes);
            let regressed_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: off1,
                prefix_crc32c: crc32c_checksum(regressed_bytes),
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init.paths.normal), &regressed_meta);
            write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), &regressed_meta);

            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC5a: LTM2 committed_len regression behind DB hw must fail closed");
            assert_corrupt_committed_frame(err, "AC5a same-segment offset regression");
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                db_hw
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);

            // Subcase 5b: Rotated `sealed[0].sealed_len < hw.byte_offset` regression.
            let s1_frame = encode_postcard_frame(
                FRAME_FLAG_NONE,
                "ac5b:s1:0",
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("ac5b-s1-0", "2026-11-23T16:21:00Z"),
            )
            .unwrap();
            write_bytes_0600(&init.paths.normal.join(segment_log_file_name(1)), &s1_frame);
            let bad_sealed_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 1,
                committed_len: s1_frame.len() as u64,
                prefix_crc32c: crc32c_checksum(&s1_frame),
                sealed: vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: off1, // regressed below DB hw `off3`!
                    sealed_crc32c: crc32c_checksum(regressed_bytes),
                }],
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init.paths.normal), &bad_sealed_meta);
            write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), &bad_sealed_meta);

            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC5b: sealed_len < DB hw must fail closed");
            assert_corrupt_committed_frame(err, "AC5b sealed_len regression");
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                db_hw
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);

            // Subcase 5c: `min_live_seg` leapfrogged ahead of unconsumed DB `hw.segment_id`.
            let leapfrog_meta = LaneTailMeta {
                min_live_seg: 1,
                active_seg: 1,
                committed_len: s1_frame.len() as u64,
                prefix_crc32c: crc32c_checksum(&s1_frame),
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init.paths.normal), &leapfrog_meta);
            write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), &leapfrog_meta);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC5c: min_live_seg > hw.segment_id must fail closed");
            assert_corrupt_committed_frame(err, "AC5c min_live_seg leapfrog");
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                db_hw
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);

            // Subcase 5d: Lane reset to 0 segments + virgin empty LTM2 while DB hw > (0, 0).
            fs::remove_file(&seg0).unwrap();
            fs::remove_file(init.paths.normal.join(segment_log_file_name(1))).unwrap();
            let empty_ltm2 = LaneTailMeta::initial_empty().encode().unwrap();
            write_bytes_0600(&tail_meta_path(&init.paths.normal), &empty_ltm2);
            write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), &empty_ltm2);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC5d: reset empty lane behind non-zero DB hw must fail closed");
            assert_corrupt_committed_frame(err, "AC5d empty reset regression");
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                db_hw
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);

            // Subcase 5e: `active_seg < hw.segment_id` segment-ID regression (`DB hw = (2, 0)`,
            // `LTM2` rolled back to `active_seg = 0`).
            let _ = store
                .append_capture_batch(
                    &[],
                    Some((
                        CaptureQueueLane::Normal,
                        CaptureQueueHighwater {
                            segment_id: 2,
                            byte_offset: 0,
                        },
                    )),
                )
                .await
                .unwrap();
            let hw_seg2 = store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap();
            assert_eq!(hw_seg2.segment_id, 2);
            write_bytes_0600(&seg0, &orig_seg0);
            let old_seg0_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 0,
                committed_len: orig_seg0.len() as u64,
                prefix_crc32c: crc32c_checksum(&orig_seg0),
                sealed: Vec::new(),
            }
            .encode()
            .unwrap();
            write_bytes_0600(&tail_meta_path(&init.paths.normal), &old_seg0_meta);
            write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), &old_seg0_meta);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let err = drain_capture_queue(repo.path(), &store)
                .await
                .expect_err("AC5e: active_seg < DB hw.segment_id must fail closed");
            assert_corrupt_committed_frame(err, "AC5e active_seg regression");
            assert_eq!(store.write_transaction_count(), tx_before);
            assert_eq!(
                store
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .unwrap(),
                hw_seg2
            );
            assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before);
        }
    }

    #[tokio::test]
    async fn single_writer_lock_contention_and_active_probe() {
        // `RB-09b` AC6 & AC7:
        // - AC6: A second consumer contending for `writer.lock` fails acquisition
        //   and returns `Ok(CaptureDrainReport { acquired_writer_lock: false, ..0 })`
        //   immediately with ZERO side effects (0 DB writes, 0 highwater/disk mutations).
        // - AC7: `is_writer_active()` reuses the exact same `writer.lock` and
        //   accurately reports `false` before creation (without creating `writer.lock`),
        //   `true` while held, and `false` immediately after release.
        let repo = tempdir().expect("tempdir");

        // Before queue initialization: `is_writer_active` returns `Ok(false)` without creating dirs.
        assert!(!is_writer_active(repo.path()).expect("probe uninitialized repo"));
        assert!(!repo.path().join(PRIVATE_SUBDIR).exists());

        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let lock_file = writer_lock_path(&init.paths.root);
        assert!(!lock_file.exists());
        assert!(
            !is_writer_active(repo.path()).expect("probe fresh queue before writer.lock exists")
        );
        assert!(
            !lock_file.exists(),
            "AC7: is_writer_active() must not create writer.lock when absent"
        );

        let store = RedbCaptureStore::create(
            repo.path()
                .join(PRIVATE_SUBDIR)
                .join("capture.gen-0001.redb"),
        )
        .await
        .expect("create RedbCaptureStore");
        let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

        // Enqueue 1 priority + 2 normal frames before acquiring lock.
        let _ = append_postcard_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb09b:ac6:prio:0",
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &make_session_payload("ac6-sess-0", CaptureSessionState::Stopped, 1_700_002_000),
        )
        .unwrap();
        for idx in 0..2u32 {
            let _ = append_postcard_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &format!("rb09b:ac6:norm:{idx}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload(&format!("ac6-norm-{idx}"), "2026-11-23T17:00:00Z"),
            )
            .unwrap();
        }

        // Consumer 1 holds `writer.lock` on a separate OS thread so contention
        // exercises a genuinely independent descriptor + thread owner.
        let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder_repo = repo.path().to_path_buf();
        let holder = std::thread::spawn(move || {
            let guard = try_acquire_writer_lock(&holder_repo)
                .expect("holder acquire writer.lock")
                .expect("writer.lock must be uncontended");
            locked_tx.send(()).expect("signal locked");
            release_rx.recv().expect("wait for release signal");
            drop(guard);
        });
        locked_rx.recv().expect("wait for holder lock");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::symlink_metadata(&lock_file)
                .expect("stat writer.lock")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o600,
                "writer.lock must be created with 0600 permissions"
            );
        }

        // AC7: `is_writer_active()` detects the active holder via the same `writer.lock`.
        assert!(
            is_writer_active(repo.path()).expect("probe active writer"),
            "AC7 violated: is_writer_active() must return true while writer.lock is held"
        );
        assert!(
            try_acquire_writer_lock(repo.path())
                .expect("contended try_acquire_writer_lock")
                .is_none(),
            "contended try_acquire_writer_lock must return Ok(None)"
        );

        // AC6: Second consumer contending for `writer.lock` returns `Ok`
        // immediately with ZERO side effects.
        let tree_before = snapshot_queue_tree_full(&init.paths.root);
        let tx_before = store.write_transaction_count();
        let contended_1 = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("AC6: contended drain_capture_queue must return Ok");
        let contended_2 = CaptureQueueWriter::in_storage_dir(repo.path())
            .drain_once(&spy)
            .await
            .expect("AC6: contended CaptureQueueWriter::drain_once must return Ok");

        for (label, rep) in [
            ("drain_capture_queue", contended_1),
            ("drain_once", contended_2),
        ] {
            assert!(
                !rep.acquired_writer_lock,
                "{label} must not acquire contended lock"
            );
            assert_eq!(rep.priority_batches, 0);
            assert_eq!(rep.normal_batches, 0);
            assert_eq!(rep.applied_frames, 0);
            assert_eq!(rep.deduplicated_frames, 0);
            assert_eq!(rep.reclaimed_segments, 0);
            assert_eq!(rep.recovery_delegations, 0);
            assert!(rep.highwater.is_none());
        }
        assert!(spy.recorded_calls().is_empty());
        assert_eq!(store.write_transaction_count(), tx_before);
        assert_eq!(
            store
                .get_queue_highwater(CaptureQueueLane::Priority)
                .await
                .unwrap(),
            CaptureQueueHighwater::default()
        );
        assert_eq!(
            store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .unwrap(),
            CaptureQueueHighwater::default()
        );
        assert_eq!(
            snapshot_queue_tree_full(&init.paths.root),
            tree_before,
            "AC6 violated: contended consumer must cause ZERO disk side effects"
        );

        // Release Consumer 1's lock -> `is_writer_active()` flips immediately to `false`
        // (even though `writer.lock` remains on disk), and Consumer 2 succeeds!
        release_tx.send(()).expect("signal holder release");
        holder.join().expect("join holder thread");

        assert!(lock_file.exists());
        assert!(
            !is_writer_active(repo.path()).expect("probe after release"),
            "AC7 violated: is_writer_active() must return false immediately after release"
        );

        let drained = drain_capture_queue(repo.path(), &spy)
            .await
            .expect("consumer 2 must drain cleanly after lock release");
        assert!(drained.acquired_writer_lock);
        assert_eq!(drained.priority_batches, 1);
        assert_eq!(drained.normal_batches, 1);
        assert_eq!(drained.applied_frames, 3);
        assert!(store.get_session("ac6-sess-0").await.unwrap().is_some());
        assert_eq!(
            store
                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            !is_writer_active(repo.path()).expect("probe after drain_capture_queue exit"),
            "writer.lock must be released after drain_capture_queue returns"
        );
    }

    /// Helper: enqueue 4 frames into `seg 0` via `append_postcard_frame`, then
    /// seal `seg 0` into `LTM2.sealed[]` with rotated `seg 1` (1 frame), and
    /// optionally rotate again into `seg 2` (1 frame).
    /// Returns `(seg0_frame_end_offsets, seg0_len, seg1_len)` where `seg0_frame_end_offsets`
    /// has 4 exact complete-frame boundary offsets (`[off1, off2, off3, off4 == seg0_len]`).
    fn seed_rotated_lane_segments(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        prefix: &str,
        also_rotate_to_seg2: bool,
    ) -> (Vec<u64>, u64, u64) {
        let flags = match lane {
            CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
            CaptureQueueLane::Normal => FRAME_FLAG_NONE,
        };
        let payload_for = |tag: &str, seq: i64| match lane {
            CaptureQueueLane::Priority => {
                make_session_payload(tag, CaptureSessionState::Stopped, 1_700_010_000 + seq)
            }
            CaptureQueueLane::Normal => make_audit_payload(tag, "2026-11-24T03:00:00Z"),
        };

        let mut seg0_offsets = Vec::with_capacity(4);
        for i in 0..4u32 {
            let out = append_postcard_frame(
                storage_dir,
                lane,
                flags,
                &format!("{prefix}:s0:{i}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("{prefix}-s0-{i}"), i64::from(i)),
            )
            .expect("enqueue frame into seg 0");
            assert_eq!(out.seg_id, 0);
            seg0_offsets.push(out.meta.committed_len);
        }
        let seg0_len = *seg0_offsets.last().expect("non-empty seg0_offsets");
        let lane_dir = storage_dir
            .join(PRIVATE_SUBDIR)
            .join(CAPTURE_QUEUE_SUBDIR)
            .join(lane_subdir_name(lane));
        let seg0_bytes = fs::read(lane_dir.join(segment_log_file_name(0))).expect("read seg 0");
        assert_eq!(seg0_bytes.len() as u64, seg0_len);
        let seg0_crc = crc32c_checksum(&seg0_bytes);

        let seg1_bytes = encode_postcard_frame(
            flags,
            &format!("{prefix}:s1:0"),
            CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
            &payload_for(&format!("{prefix}-s1-0"), 10),
        )
        .expect("encode seg 1 frame");
        let seg1_len = seg1_bytes.len() as u64;
        let seg1_crc = crc32c_checksum(&seg1_bytes);
        write_bytes_0600(&lane_dir.join(segment_log_file_name(1)), &seg1_bytes);

        let meta = if also_rotate_to_seg2 {
            let seg2_bytes = encode_postcard_frame(
                flags,
                &format!("{prefix}:s2:0"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("{prefix}-s2-0"), 20),
            )
            .expect("encode seg 2 frame");
            let seg2_len = seg2_bytes.len() as u64;
            let seg2_crc = crc32c_checksum(&seg2_bytes);
            write_bytes_0600(&lane_dir.join(segment_log_file_name(2)), &seg2_bytes);
            LaneTailMeta {
                min_live_seg: 0,
                active_seg: 2,
                committed_len: seg2_len,
                prefix_crc32c: seg2_crc,
                sealed: vec![
                    SealedSegmentMeta {
                        seg_id: 0,
                        sealed_len: seg0_len,
                        sealed_crc32c: seg0_crc,
                    },
                    SealedSegmentMeta {
                        seg_id: 1,
                        sealed_len: seg1_len,
                        sealed_crc32c: seg1_crc,
                    },
                ],
            }
        } else {
            LaneTailMeta {
                min_live_seg: 0,
                active_seg: 1,
                committed_len: seg1_len,
                prefix_crc32c: seg1_crc,
                sealed: vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: seg0_len,
                    sealed_crc32c: seg0_crc,
                }],
            }
        };
        let encoded_meta = meta.encode().expect("encode rotated LTM2");
        write_bytes_0600(&tail_meta_path(&lane_dir), &encoded_meta);
        write_bytes_0600(&tail_meta_bak_path(&lane_dir), &encoded_meta);

        (seg0_offsets, seg0_len, seg1_len)
    }

    /// Helper: assert that every frame in `file_path` decodes cleanly via
    /// `FrameStreamDecoder` and terminates with `CleanEof { offset: expected_len }`
    /// after exactly `expected_frames` valid frames.
    fn assert_segment_decodes_clean_eof(
        file_path: &Path,
        expected_frames: usize,
        expected_len: u64,
    ) {
        let mut f = File::open(file_path).expect("open segment for FrameStreamDecoder check");
        let len = f.metadata().expect("stat segment").len();
        assert_eq!(len, expected_len);
        let mut decoder = FrameStreamDecoder::with_known_len(&mut f, 0, len);
        let mut count = 0usize;
        loop {
            match decoder
                .next_frame()
                .expect("truncated complete-frame segment must decode without stream error")
            {
                FrameStreamItem::Frame { frame, .. } => {
                    let _: CaptureBatchPayload =
                        decode_postcard_payload(&frame, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION)
                            .expect("frame inside truncated boundary must decode cleanly");
                    count += 1;
                }
                FrameStreamItem::CleanEof { offset } => {
                    assert_eq!(offset, expected_len);
                    break;
                }
            }
        }
        assert_eq!(count, expected_frames);
    }

    #[tokio::test]
    async fn consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing() {
        // `RB-09d` AC1–AC4 (`ADR-RB-05` #4):
        // Authenticate EVERY un-reclaimed rotated segment (`min_live_seg <= seg < active_seg`)
        // against `LTM2.sealed[]` before consuming frames or reclaiming segments:
        // - AC1: Exact complete-frame boundary truncation (still 100% `CleanEof`!) rejected
        //        via `actual_len != sealed_len` -> `CorruptCommittedFrame` (`LBR-IO-001`).
        // - AC2: Single-byte bitflip in rotated segment rejected via `sealed_crc32c` mismatch
        //        -> `CorruptCommittedFrame` (`LBR-IO-001`).
        // - AC3: Missing un-reclaimed rotated segment registered in `LTM2.sealed[]` rejected
        //        -> `CorruptCommittedFrame` (`LBR-IO-001`).
        // - AC4: Trailing extra bytes (`actual_len > sealed_len`, both garbage and extra valid
        //        frame!) rejected -> `CorruptCommittedFrame` (`LBR-IO-001`).
        // Verified on BOTH `normal/` and `priority/` lanes, covering unconsumed (`hw == (0,0)`),
        // partially-consumed (`hw == (0, off1)`), multi-rotated (`sealed[0..=1]`), and
        // consumed-but-un-reclaimed (`min_live_seg = 0 < hw.segment_id = 1`) rotated segments,
        // plus cross-lane fail-closed atomicity (0 DB tx, 0 highwater advance, 0 reclamation,
        // full queue tree including `writer.lock` byte-identical).
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let other_lane = match lane {
                CaptureQueueLane::Normal => CaptureQueueLane::Priority,
                CaptureQueueLane::Priority => CaptureQueueLane::Normal,
            };
            let lane_label = lane_subdir_name(lane);

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            bootstrap_writer_lock(&init.paths.root);
            let lane_dir = match lane {
                CaptureQueueLane::Normal => &init.paths.normal,
                CaptureQueueLane::Priority => &init.paths.priority,
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");
            let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

            // Seed a healthy frame on the OTHER lane too, proving preflight rejects
            // rotated-segment corruption BEFORE committing ANY batch on either lane!
            let other_flags = match other_lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let other_payload = match other_lane {
                CaptureQueueLane::Priority => make_session_payload(
                    &format!("rb09d-other-prio-{lane_label}"),
                    CaptureSessionState::Stopped,
                    1_700_020_000,
                ),
                CaptureQueueLane::Normal => make_audit_payload(
                    &format!("rb09d-other-norm-{lane_label}"),
                    "2026-11-24T03:01:00Z",
                ),
            };
            let _ = append_postcard_frame(
                repo.path(),
                other_lane,
                other_flags,
                &format!("rb09d:other:{lane_label}"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &other_payload,
            )
            .expect("enqueue healthy frame on sibling lane");

            // Seed seg 0 (4 frames) -> seg 1 (1 frame) -> seg 2 (1 frame), so
            // `min_live_seg = 0`, `active_seg = 2`, `sealed = [0, 1]`.
            let (seg0_offsets, seg0_len, _seg1_len) =
                seed_rotated_lane_segments(repo.path(), lane, &format!("rb09d-{lane_label}"), true);
            let meta_init = read_converged_lane_tail_meta(lane_dir).expect("read LTM2");
            assert_eq!(meta_init.min_live_seg, 0);
            assert_eq!(meta_init.active_seg, 2);
            assert_eq!(meta_init.sealed.len(), 2);
            assert_eq!(meta_init.sealed[0].seg_id, 0);
            assert_eq!(meta_init.sealed[0].sealed_len, seg0_len);
            assert_eq!(meta_init.sealed[1].seg_id, 1);

            let seg0_path = lane_dir.join(segment_log_file_name(0));
            let seg1_path = lane_dir.join(segment_log_file_name(1));
            let orig_seg0 = fs::read(&seg0_path).expect("read seg 0");
            let orig_seg1 = fs::read(&seg1_path).expect("read seg 1");
            assert_eq!(orig_seg0.len() as u64, seg0_len);

            // Helper closure to assert fail-closed zero-mutation invariants.
            let assert_fail_closed =
                |err: StorageBackendError,
                 label: &str,
                 tx_before: u64,
                 calls_before: usize,
                 expected_hw: CaptureQueueHighwater,
                 tree_before: &std::collections::BTreeMap<String, Vec<u8>>| {
                    assert_corrupt_committed_frame(err, &format!("{lane_label} {label}"));
                    assert_eq!(
                        store.write_transaction_count(),
                        tx_before,
                        "{lane_label} {label}: must execute 0 DB write transactions"
                    );
                    assert_eq!(
                        spy.recorded_calls().len(),
                        calls_before,
                        "{lane_label} {label}: must invoke 0 append_capture_batch calls"
                    );
                    assert_eq!(
                        snapshot_queue_tree_full(&init.paths.root),
                        *tree_before,
                        "{lane_label} {label}: must leave entire queue tree (incl. writer.lock & LTM2) byte-identical"
                    );
                    let _ = expected_hw;
                };

            // -----------------------------------------------------------------
            // AC1: Exact complete-frame boundary truncation (`CleanEof` stream!)
            // Truncate `events-00000000.log` cleanly to frame #2 (`seg0_offsets[1]`).
            // -----------------------------------------------------------------
            let trunc_len = seg0_offsets[1];
            assert!(trunc_len > 0 && trunc_len < seg0_len);
            write_bytes_0600(&seg0_path, &orig_seg0[..trunc_len as usize]);
            // Prove the truncated file STILL decodes 100% cleanly as 2 valid frames + `CleanEof`!
            assert_segment_decodes_clean_eof(&seg0_path, 2, trunc_len);

            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let calls_before = spy.recorded_calls().len();
            let err = drain_capture_queue(repo.path(), &spy)
                .await
                .expect_err("AC1: exact complete-frame truncation on rotated seg must fail closed");
            assert_fail_closed(
                err,
                "AC1 complete-frame CleanEof truncation (seg 0)",
                tx_before,
                calls_before,
                CaptureQueueHighwater::default(),
                &tree_before,
            );
            assert!(
                seg0_path.exists(),
                "AC1: truncated seg 0 must NOT be reclaimed"
            );
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater::default()
            );
            assert_eq!(
                store.get_queue_highwater(other_lane).await.unwrap(),
                CaptureQueueHighwater::default(),
                "AC1: sibling lane must not advance when preflight rejects rotated seg"
            );
            write_bytes_0600(&seg0_path, &orig_seg0);

            // -----------------------------------------------------------------
            // AC2: Single-byte bitflip on rotated segment (test BOTH seg 0 AND seg 1,
            // where `hw == (0, 0)` has not even reached seg 1 yet!)
            // -----------------------------------------------------------------
            for (tag, target_path, orig_bytes) in [
                ("seg 0 bitflip", &seg0_path, &orig_seg0),
                ("seg 1 bitflip (ahead of hw)", &seg1_path, &orig_seg1),
            ] {
                let mut flipped = orig_bytes.clone();
                let flip_idx = flipped.len() / 2;
                flipped[flip_idx] ^= 0x01;
                assert_eq!(flipped.len(), orig_bytes.len());
                write_bytes_0600(target_path, &flipped);

                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let err = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect_err("AC2: single-byte bitflip on rotated seg must fail closed");
                assert_fail_closed(
                    err,
                    &format!("AC2 {tag}"),
                    tx_before,
                    calls_before,
                    CaptureQueueHighwater::default(),
                    &tree_before,
                );
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater::default()
                );
                assert_eq!(
                    store.get_queue_highwater(other_lane).await.unwrap(),
                    CaptureQueueHighwater::default()
                );
                write_bytes_0600(target_path, orig_bytes);
            }

            // -----------------------------------------------------------------
            // AC3: Missing un-reclaimed rotated segment registered in `LTM2.sealed[]`
            // (test BOTH seg 0 AND seg 1).
            // -----------------------------------------------------------------
            for (tag, target_path, orig_bytes) in [
                ("seg 0 missing", &seg0_path, &orig_seg0),
                ("seg 1 missing", &seg1_path, &orig_seg1),
            ] {
                fs::remove_file(target_path).expect("delete rotated seg");
                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let err = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect_err("AC3: missing rotated seg must fail closed");
                assert_fail_closed(
                    err,
                    &format!("AC3 {tag}"),
                    tx_before,
                    calls_before,
                    CaptureQueueHighwater::default(),
                    &tree_before,
                );
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater::default()
                );
                write_bytes_0600(target_path, orig_bytes);
            }

            // -----------------------------------------------------------------
            // AC4: Trailing extra bytes (`actual_len > sealed_len`) — BOTH arbitrary
            // garbage AND an extra 100%-valid postcard frame (`CleanEof` stream!).
            // -----------------------------------------------------------------
            let extra_valid_frame = encode_postcard_frame(
                match lane {
                    CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                    CaptureQueueLane::Normal => FRAME_FLAG_NONE,
                },
                &format!("rb09d:{lane_label}:rogue-extra"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &make_audit_payload("rb09d-rogue-extra", "2026-11-24T03:02:00Z"),
            )
            .expect("encode extra valid frame");
            for (tag, suffix) in [
                (
                    "trailing garbage bytes",
                    b"TRAILING_GARBAGE_BYTES".as_slice(),
                ),
                ("trailing extra valid frame", extra_valid_frame.as_slice()),
            ] {
                let mut extended = orig_seg0.clone();
                extended.extend_from_slice(suffix);
                write_bytes_0600(&seg0_path, &extended);
                if tag == "trailing extra valid frame" {
                    assert_segment_decodes_clean_eof(&seg0_path, 5, extended.len() as u64);
                }

                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let err = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect_err("AC4: trailing extra bytes on rotated seg must fail closed");
                assert_fail_closed(
                    err,
                    &format!("AC4 {tag}"),
                    tx_before,
                    calls_before,
                    CaptureQueueHighwater::default(),
                    &tree_before,
                );
                assert!(
                    seg0_path.exists(),
                    "AC4: tampered seg 0 must NOT be reclaimed"
                );
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater::default()
                );
                write_bytes_0600(&seg0_path, &orig_seg0);
            }

            // -----------------------------------------------------------------
            // Additional boundary proof for ALL `min_live_seg <= seg < active_seg`:
            // Even when DB highwater has ALREADY moved past `seg 0` (`hw = (1, 0)`)
            // while `seg 0` is still un-reclaimed (`min_live_seg = 0`), mutating,
            // truncating, deleting, or appending trailing bytes to `seg 0` MUST
            // STILL fail closed with `CorruptCommittedFrame` before reclaiming!
            // -----------------------------------------------------------------
            let hw_seg1 = CaptureQueueHighwater {
                segment_id: 1,
                byte_offset: 0,
            };
            let _ = store
                .append_capture_batch(&[], Some((lane, hw_seg1)))
                .await
                .expect("advance DB hw to (1, 0) while seg 0 remains un-reclaimed");
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_seg1);

            // (a) Truncate already-consumed un-reclaimed `seg 0` to `CleanEof` boundary.
            write_bytes_0600(&seg0_path, &orig_seg0[..trunc_len as usize]);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let calls_before = spy.recorded_calls().len();
            let err = drain_capture_queue(repo.path(), &spy)
                .await
                .expect_err("consumed-but-un-reclaimed seg 0 truncation must fail closed");
            assert_fail_closed(
                err,
                "consumed-unreclaimed seg 0 truncation",
                tx_before,
                calls_before,
                hw_seg1,
                &tree_before,
            );

            // (b) Delete already-consumed un-reclaimed `seg 0`.
            fs::remove_file(&seg0_path).unwrap();
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let calls_before = spy.recorded_calls().len();
            let err = drain_capture_queue(repo.path(), &spy)
                .await
                .expect_err("consumed-but-un-reclaimed seg 0 missing must fail closed");
            assert_fail_closed(
                err,
                "consumed-unreclaimed seg 0 missing",
                tx_before,
                calls_before,
                hw_seg1,
                &tree_before,
            );

            // (c) Append trailing bytes to already-consumed un-reclaimed `seg 0`.
            let mut extended = orig_seg0.clone();
            extended.extend_from_slice(&extra_valid_frame);
            write_bytes_0600(&seg0_path, &extended);
            let tree_before = snapshot_queue_tree_full(&init.paths.root);
            let tx_before = store.write_transaction_count();
            let calls_before = spy.recorded_calls().len();
            let err = drain_capture_queue(repo.path(), &spy)
                .await
                .expect_err("consumed-but-un-reclaimed seg 0 trailing bytes must fail closed");
            assert_fail_closed(
                err,
                "consumed-unreclaimed seg 0 trailing bytes",
                tx_before,
                calls_before,
                hw_seg1,
                &tree_before,
            );

            // Restore untouched `seg 0` -> consumer now drains cleanly and reclaims `seg 0` & `seg 1`!
            write_bytes_0600(&seg0_path, &orig_seg0);
            let ok_rep = drain_capture_queue(repo.path(), &spy)
                .await
                .expect("restored segments must drain and reclaim cleanly");
            assert!(ok_rep.acquired_writer_lock);
            assert_eq!(ok_rep.reclaimed_segments, 2);
            assert!(!seg0_path.exists());
            assert!(!seg1_path.exists());
            let final_meta = read_converged_lane_tail_meta(lane_dir).unwrap();
            assert_eq!(final_meta.min_live_seg, 2);
            assert_eq!(final_meta.active_seg, 2);
            assert!(final_meta.sealed.is_empty());
        }
    }

    #[tokio::test]
    async fn reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan() {
        // `RB-09d` AC5 (`ADR-RB-05` #4 ③):
        // Verify on BOTH `normal/` and `priority/`:
        // 1. Crash injection via `AfterReclaimMetaBeforeUnlink` inside the real
        //    consumer (`drain_locked_at_root`): consumer commits dual `LTM2`
        //    (`MIN_LIVE_SEG = S + 1`, removing `sealed[S]`) strictly BEFORE
        //    `unlink("events-<S>.log")` and crashes before unlinking.
        //    Restart (Scenario A: with remaining unconsumed frames in `S + 1`)
        //    ingests remaining frames with 0 duplicates, recognizes `S < MIN_LIVE_SEG`
        //    as a reclaimed crash orphan, safely executes `unlink` + `fsync_dir`
        //    without false alarms and without touching `quarantine/`.
        // 2. Scenario B (0 remaining frames in `S + 1` on restart): crash right
        //    after updating `LTM2` (`MIN_LIVE_SEG = S + 1`) and before `unlink(S)`
        //    when `S + 1` is already completely drained -> restart runs 0 DB write
        //    transactions and still cleans crash orphan `S` via `unlink` + `fsync_dir`
        //    with 0 quarantine and 0 false alarms.
        // 3. Healthy non-crashing reclaim strict durability order proof:
        //    `AcquireAppendLock < MetaRename(tail.meta) < FsyncLaneDirAfterMeta(tail.meta)
        //     < MetaRename(tail.meta.bak) < FsyncLaneDirAfterMeta(tail.meta.bak)
        //     < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim < ReleaseAppendLock`.
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);

            // =================================================================
            // Scenario A: Crash after LTM2 commit before unlink(seg 0), WITH
            // remaining unconsumed frames in seg 1.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => &init.paths.normal,
                    CaptureQueueLane::Priority => &init.paths.priority,
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let (_seg0_offsets, _seg0_len, seg1_len) = seed_rotated_lane_segments(
                    repo.path(),
                    lane,
                    &format!("rb09d-ac5a-{lane_label}"),
                    false,
                );
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                let seg1_path = lane_dir.join(segment_log_file_name(1));
                assert!(seg0_path.exists() && seg1_path.exists());

                // Run real consumer with `AfterReclaimMetaBeforeUnlink` crash injection.
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .expect("acquire writer.lock")
                    .expect("writer.lock free");
                let mut crash_obs = WriterObserver::with_reclaim_crash_point(
                    SegmentCrashPoint::AfterReclaimMetaBeforeUnlink,
                );
                let crash_err = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("AC5: must crash after LTM2 reclaim commit and before unlink(seg 0)");
                assert!(
                    matches!(crash_err, StorageBackendError::Io { .. }),
                    "expected injected Io crash, got {crash_err:?}"
                );
                drop(lock);

                // Verify post-crash state: BOTH `tail.meta` and `tail.meta.bak`
                // already advanced `MIN_LIVE_SEG = 1` & removed `sealed[0]`, while
                // `events-00000000.log` STILL exists on disk (unlink never ran!).
                assert!(
                    seg0_path.exists(),
                    "AC5 ({lane_label}): events-00000000.log must still exist right after pre-unlink crash"
                );
                let primary_bytes = fs::read(tail_meta_path(lane_dir)).unwrap();
                let backup_bytes = fs::read(tail_meta_bak_path(lane_dir)).unwrap();
                assert_eq!(primary_bytes, backup_bytes);
                let crashed_meta = LaneTailMeta::decode(&primary_bytes).unwrap();
                assert_eq!(crashed_meta.min_live_seg, 1);
                assert_eq!(crashed_meta.active_seg, 1);
                assert!(crashed_meta.sealed.is_empty());
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert!(
                    crash_obs
                        .segment_events
                        .contains(&SegmentOpEvent::FsyncLaneDirAfterMeta {
                            lane: lane_label,
                            target_name: QUEUE_TAIL_META_BAK_FILE,
                        }),
                    "AC5 ({lane_label}): dual LTM2 commit must finish before pre-unlink crash"
                );
                assert!(
                    !crash_obs.segment_events.iter().any(|e| matches!(
                        e,
                        SegmentOpEvent::ReclaimSegmentUnlink { .. }
                            | SegmentOpEvent::FsyncLaneDirAfterReclaim { .. }
                    )),
                    "AC5 ({lane_label}): unlink must NOT have executed before injected crash"
                );

                // Restart consumer: drains seg 1 (1 frame, 0 dups), recognizes
                // `0 < MIN_LIVE_SEG (1)` as reclaimed crash orphan, safely executes
                // `unlink` + `fsync_dir` with 0 false alarms and 0 quarantine!
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .expect("acquire writer.lock for restart")
                    .expect("writer.lock free");
                let mut restart_obs = WriterObserver::new();
                let restart_rep = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut restart_obs,
                )
                .await
                .expect("AC5: restart after pre-unlink crash must succeed with zero false alarms");
                drop(lock);

                assert!(restart_rep.acquired_writer_lock);
                assert_eq!(restart_rep.applied_frames, 1);
                assert_eq!(restart_rep.deduplicated_frames, 0);
                assert_eq!(restart_rep.recovery_delegations, 1);
                assert!(
                    !seg0_path.exists(),
                    "AC5 ({lane_label}): crash orphan events-00000000.log must be unlinked on restart"
                );
                assert!(seg1_path.exists());
                assert_eq!(
                    fs::read_dir(&init.paths.quarantine).unwrap().count(),
                    0,
                    "AC5 ({lane_label}): reclaimed crash orphan must NEVER be quarantined"
                );
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: seg1_len,
                    }
                );
                let pos_lock = restart_obs
                    .segment_events
                    .iter()
                    .position(|e| *e == SegmentOpEvent::AcquireAppendLock)
                    .expect("restart orphan cleanup must acquire append.lock");
                let pos_orphan_unlink = restart_obs
                    .segment_events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::ReclaimedOrphanUnlink {
                            lane: lane_label,
                            seg_id: 0,
                        }
                    })
                    .expect("restart must unlink reclaimed orphan seg 0");
                let pos_orphan_fsync = restart_obs
                    .segment_events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterReclaimedOrphan { lane: lane_label }
                    })
                    .expect("restart must fsync lane_dir after unlinking reclaimed orphan");
                let pos_unlock = restart_obs
                    .segment_events
                    .iter()
                    .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
                    .expect("restart must release append.lock");
                assert!(
                    pos_lock < pos_orphan_unlink
                        && pos_orphan_unlink < pos_orphan_fsync
                        && pos_orphan_fsync < pos_unlock,
                    "AC5 ({lane_label}): orphan cleanup must execute AcquireAppendLock < unlink < fsync_dir < ReleaseAppendLock; got {:?}",
                    restart_obs.segment_events
                );

                // Verify all 5 frames (4 from seg 0 + 1 from seg 1) persisted cleanly with 0 dups.
                match lane {
                    CaptureQueueLane::Normal => {
                        assert_eq!(
                            store
                                .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                                .await
                                .unwrap()
                                .len(),
                            5
                        );
                    }
                    CaptureQueueLane::Priority => {
                        assert_eq!(
                            store.list_sessions_by_started_desc().await.unwrap().len(),
                            5
                        );
                    }
                }
            }

            // =================================================================
            // Scenario B: Crash after LTM2 commit before unlink(seg 0), WITH 0
            // remaining unconsumed frames in seg 1 -> restart runs 0 DB tx and
            // cleanly unlinks + fsync_dir the reclaimed crash orphan.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => &init.paths.normal,
                    CaptureQueueLane::Priority => &init.paths.priority,
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let (_seg0_offsets, _seg0_len, seg1_len) = seed_rotated_lane_segments(
                    repo.path(),
                    lane,
                    &format!("rb09d-ac5b-{lane_label}"),
                    false,
                );
                let seg0_path = lane_dir.join(segment_log_file_name(0));

                // Advance DB highwater to `(1, seg1_len)` (all frames already
                // consumed while `seg 0` is still un-reclaimed `min_live_seg = 0`).
                let drained_hw = CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: seg1_len,
                };
                let _ = store
                    .append_capture_batch(&[], Some((lane, drained_hw)))
                    .await
                    .unwrap();

                // Crash consumer during reclamation of `seg 0` after dual LTM2 commit
                // and strictly before `unlink("events-00000000.log")`.
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut crash_obs = WriterObserver::with_reclaim_crash_point(
                    SegmentCrashPoint::AfterReclaimMetaBeforeUnlink,
                );
                let _ = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("AC5b: must crash after LTM2 reclaim commit before unlink(seg 0)");
                drop(lock);
                assert!(seg0_path.exists());
                assert_eq!(
                    read_converged_lane_tail_meta(lane_dir)
                        .unwrap()
                        .min_live_seg,
                    1
                );

                // Restart with ZERO remaining frames to consume: 0 DB writes,
                // orphan `seg 0` unlinked + `fsync_dir`, 0 quarantine, 0 false alarms!
                let tx_before = store.write_transaction_count();
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut restart_obs = WriterObserver::new();
                let restart_rep = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut restart_obs,
                )
                .await
                .expect("AC5b: 0-frame restart must clean crash orphan without false alarms");
                drop(lock);

                assert_eq!(store.write_transaction_count(), tx_before);
                assert_eq!(restart_rep.applied_frames, 0);
                assert_eq!(restart_rep.recovery_delegations, 1);
                assert!(!seg0_path.exists());
                assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
                assert!(restart_obs.segment_events.contains(
                    &SegmentOpEvent::ReclaimedOrphanUnlink {
                        lane: lane_label,
                        seg_id: 0,
                    }
                ));
                assert!(restart_obs.segment_events.contains(
                    &SegmentOpEvent::FsyncLaneDirAfterReclaimedOrphan { lane: lane_label }
                ));

                // Subsequent restart is a pure converged no-op.
                let noop_rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("converged no-op drain");
                assert_eq!(noop_rep.recovery_delegations, 0);
                assert_eq!(noop_rep.reclaimed_segments, 0);
                assert_eq!(store.write_transaction_count(), tx_before);
            }

            // =================================================================
            // Scenario C: Healthy non-crashing reclaim strict ordering proof
            // through real `drain_locked_at_root`:
            // `AcquireAppendLock < MetaRename(tail.meta) < FsyncLaneDirAfterMeta(tail.meta)
            //  < MetaRename(tail.meta.bak) < FsyncLaneDirAfterMeta(tail.meta.bak)
            //  < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim < ReleaseAppendLock`.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => &init.paths.normal,
                    CaptureQueueLane::Priority => &init.paths.priority,
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let _ = seed_rotated_lane_segments(
                    repo.path(),
                    lane,
                    &format!("rb09d-ac5c-{lane_label}"),
                    false,
                );
                let seg0_path = lane_dir.join(segment_log_file_name(0));

                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut healthy_obs = WriterObserver::new();
                let rep = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut healthy_obs,
                )
                .await
                .expect("AC5c: healthy drain & reclaim must succeed");
                drop(lock);

                assert_eq!(rep.reclaimed_segments, 1);
                assert_eq!(rep.applied_frames, 5);
                assert!(!seg0_path.exists());
                let primary_bytes = fs::read(tail_meta_path(lane_dir)).unwrap();
                let backup_bytes = fs::read(tail_meta_bak_path(lane_dir)).unwrap();
                assert_eq!(primary_bytes, backup_bytes);
                let final_meta = LaneTailMeta::decode(&primary_bytes).unwrap();
                assert_eq!(final_meta.min_live_seg, 1);
                assert_eq!(final_meta.active_seg, 1);
                assert!(final_meta.sealed.is_empty());

                let evs = &healthy_obs.segment_events;
                let p_lock = evs
                    .iter()
                    .position(|e| *e == SegmentOpEvent::AcquireAppendLock)
                    .expect("AcquireAppendLock");
                let p_pri_rename = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::MetaRename {
                            lane: lane_label,
                            tmp_name: QUEUE_TAIL_META_TMP_FILE,
                            target_name: QUEUE_TAIL_META_FILE,
                        }
                    })
                    .expect("MetaRename(tail.meta)");
                let p_pri_fsync = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                            lane: lane_label,
                            target_name: QUEUE_TAIL_META_FILE,
                        }
                    })
                    .expect("FsyncLaneDirAfterMeta(tail.meta)");
                let p_bak_rename = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::MetaRename {
                            lane: lane_label,
                            tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                            target_name: QUEUE_TAIL_META_BAK_FILE,
                        }
                    })
                    .expect("MetaRename(tail.meta.bak)");
                let p_bak_fsync = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                            lane: lane_label,
                            target_name: QUEUE_TAIL_META_BAK_FILE,
                        }
                    })
                    .expect("FsyncLaneDirAfterMeta(tail.meta.bak)");
                let p_unlink = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::ReclaimSegmentUnlink {
                            lane: lane_label,
                            seg_id: 0,
                        }
                    })
                    .expect("ReclaimSegmentUnlink(seg 0)");
                let p_reclaim_fsync = evs
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterReclaim {
                            lane: lane_label,
                            seg_id: 0,
                        }
                    })
                    .expect("FsyncLaneDirAfterReclaim(seg 0)");
                let p_unlock = evs
                    .iter()
                    .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
                    .expect("ReleaseAppendLock");
                assert!(
                    p_lock < p_pri_rename
                        && p_pri_rename < p_pri_fsync
                        && p_pri_fsync < p_bak_rename
                        && p_bak_rename < p_bak_fsync
                        && p_bak_fsync < p_unlink
                        && p_unlink < p_reclaim_fsync
                        && p_reclaim_fsync < p_unlock,
                    "AC5c ({lane_label}): reclaim violated strict durability ordering! events = {evs:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer() {
        // `RB-09c` AC1, AC2 & AC3 (`ADR-RB-05` #4, `GC-RB-04`):
        // - AC1: Consumer acquires `writer.lock` BEFORE `capture-queue/append.lock`,
        //   never holds `append.lock` across `CaptureStore::append_capture_batch`,
        //   and producers NEVER acquire `writer.lock`.
        // - AC2: While a concurrent producer holds `append.lock`, writes uncommitted
        //   bytes to `offset >= committed_len`, and pauses BEFORE `LTM2` commit
        //   (tested at BOTH mid-write partial bytes before `sync_data` AND full
        //   post-`sync_data` pre-`LTM2`), the concurrent consumer consumes ONLY
        //   `< committed_len` into DB and blocks on `append.lock`, NEVER
        //   quarantining in-flight bytes (`0` `LBR-AGENT-050`) and NEVER skipping
        //   or rotating the active segment.
        // - AC3: Once the paused producer resumes, finishes `sync_data` + dual
        //   `LTM2` commit, and releases `append.lock`, the consumer re-reads the
        //   advanced `LTM2` inside `append.lock`, sees `CaseAClean`, and ingests
        //   the newly committed frame 100% losslessly with `quarantine/` empty.
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_030_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(tag, "2026-11-24T04:00:00Z"),
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");

            // Seed 3 already-committed frames (`f0..f2`, `committed_len = l3`).
            let mut l3 = 0u64;
            let mut expected_payloads = Vec::new();
            let mut expected_dedups = Vec::new();
            for i in 0..3u32 {
                let dk = format!("rb09c:{lane_label}:committed:{i}");
                let pl = payload_for(&format!("rb09c-{lane_label}-c-{i}"), i64::from(i));
                let out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &dk,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &pl,
                )
                .expect("enqueue committed prefix frame");
                assert_eq!(out.seg_id, 0);
                l3 = out.meta.committed_len;
                expected_dedups.push(dk);
                expected_payloads.push(pl);
            }
            let seg0_path = lane_dir.join(segment_log_file_name(0));
            let seg1_path = lane_dir.join(segment_log_file_name(1));

            // =================================================================
            // Phase A: Producer pauses AFTER writing PARTIAL half-frame bytes
            // at `offset == l3` (before `sync_data` and before `LTM2` commit)
            // for LONGER than a single `append.lock` timeout slice (160ms > 40ms),
            // proving the consumer retries across `QueueBusy`, never exits or
            // quarantines, explicitly classifies `CaseAClean`, and persists the
            // exact payload & dedup key losslessly.
            // =================================================================
            let f3_dedup = format!("rb09c:{lane_label}:paused-partial");
            let f3_payload = payload_for(&format!("rb09c-{lane_label}-partial-3"), 3);
            let f3_bytes = encode_postcard_frame(
                flags,
                &f3_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &f3_payload,
            )
            .expect("encode f3");
            let f3_mid = f3_bytes.len() / 2;
            let expected_l4 = l3 + f3_bytes.len() as u64;

            let external_lock_held_a = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let external_lock_prod_a = Arc::clone(&external_lock_held_a);
            let (prod_paused_tx, prod_paused_rx) = std::sync::mpsc::channel::<()>();
            let (prod_release_tx, prod_release_rx) = std::sync::mpsc::channel::<()>();
            let prod_repo = repo.path().to_path_buf();
            let prod_f3 = f3_bytes.clone();
            let prod_handle = std::thread::spawn(move || {
                let mut obs = SegmentObserver::with_after_partial_frame_write_hook(
                    move |_lane_name, seg_id| {
                        assert_eq!(seg_id, 0);
                        external_lock_prod_a.store(true, std::sync::atomic::Ordering::Release);
                        prod_paused_tx.send(()).expect("signal producer paused");
                        prod_release_rx
                            .recv()
                            .expect("wait for producer release signal");
                    },
                );
                append_encoded_frame_in_storage_dir_with_observer(
                    &prod_repo,
                    lane,
                    &prod_f3,
                    QueueAppendOptions::default(),
                    &mut obs,
                )
            });

            // Wait until producer holds `append.lock` and has written partial bytes.
            prod_paused_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("producer must reach partial-write pause point");
            assert!(
                external_lock_held_a.load(std::sync::atomic::Ordering::Acquire),
                "producer pause-barrier handshake must be active while paused"
            );
            assert!(
                !is_writer_active(repo.path()).expect("probe writer.lock while producer paused"),
                "AC1 violated: producer must NEVER acquire writer.lock"
            );
            assert_eq!(
                fs::metadata(&seg0_path).unwrap().len(),
                l3 + f3_mid as u64,
                "in-flight partial frame bytes must be visible on disk at offset >= committed_len"
            );
            assert_eq!(
                read_converged_lane_tail_meta(&lane_dir)
                    .unwrap()
                    .committed_len,
                l3
            );

            // Launch concurrent consumer on a `current_thread` Tokio runtime
            // alongside a non-catching-up (`MissedTickBehavior::Skip`) 10ms timer.
            // Because `append_lock_slice = 80ms`, `ticks_before_first_busy` is
            // sampled strictly BEFORE any async retry backoff runs (`retry_n == 1`):
            // a synchronous blocking lock wait would yield 0 ticks, whereas
            // `spawn_blocking` allows >= 4 real timer ticks during the very first slice.
            let (entering_lock_tx, entering_lock_rx) = std::sync::mpsc::channel::<()>();
            let (busy_observed_tx, busy_observed_rx) = std::sync::mpsc::channel::<usize>();
            let recovery_reports = Arc::new(Mutex::new(Vec::<LaneRecoveryReport>::new()));
            let recovery_reports_hook = Arc::clone(&recovery_reports);
            let consumer_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let consumer_done_flag = Arc::clone(&consumer_done);
            let async_ticks_during_wait = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let async_ticks_task = Arc::clone(&async_ticks_during_wait);
            let async_ticks_on_retry1 = Arc::clone(&async_ticks_during_wait);
            let ticks_before_first_busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ticks_before_first_busy_hook = Arc::clone(&ticks_before_first_busy);
            let consumer_root = init.paths.root.clone();
            let consumer_root_hook = init.paths.root.clone();
            let consumer_store = store.clone();
            let consumer_ext_flag_a = Arc::clone(&external_lock_held_a);

            let consumer_handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build current_thread consumer runtime");
                rt.block_on(async move {
                    let lock = try_acquire_writer_lock_at_root(&consumer_root)
                        .expect("consumer acquire writer.lock")
                        .expect("writer.lock must be uncontended by producer");
                    let wait_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let wait_active_enter = Arc::clone(&wait_active);
                    let wait_active_exit = Arc::clone(&wait_active);
                    let wait_active_ticker = Arc::clone(&wait_active);
                    let mut obs = WriterObserver::with_recovery_hooks(
                        move |rec_lane| {
                            assert_eq!(rec_lane, lane);
                            assert!(
                                is_writer_active_at_root(&consumer_root_hook)
                                    .expect("probe writer.lock before append.lock"),
                                "AC1 violated: consumer must hold writer.lock before requesting append.lock"
                            );
                            wait_active_enter.store(true, std::sync::atomic::Ordering::Release);
                            entering_lock_tx
                                .send(())
                                .expect("signal consumer entering append.lock");
                        },
                        move |rep| {
                            wait_active_exit.store(false, std::sync::atomic::Ordering::Release);
                            recovery_reports_hook
                                .lock()
                                .expect("lock recovery reports")
                                .push(rep.clone());
                        },
                    )
                    .with_append_lock_slice(Duration::from_millis(80))
                    .with_queue_busy_retry_hook(move |retry_n| {
                        if retry_n == 1 {
                            ticks_before_first_busy_hook.store(
                                async_ticks_on_retry1.load(std::sync::atomic::Ordering::Acquire),
                                std::sync::atomic::Ordering::Release,
                            );
                        }
                        if retry_n == 2 {
                            let _ = busy_observed_tx.send(retry_n);
                        }
                    });
                    let spy = SpyCaptureStore::new_with_lock_tracking(
                        consumer_store.clone(),
                        consumer_root.clone(),
                        obs.consumer_holds_append_lock_handle(),
                        consumer_ext_flag_a,
                    );
                    let drain_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let drain_finished_ticker = Arc::clone(&drain_finished);
                    let drain_fut = async {
                        let rep = drain_locked_at_root(
                            &consumer_root,
                            &spy,
                            QueueWriterOptions::default(),
                            &lock,
                            &mut obs,
                        )
                        .await
                        .expect("consumer drain must survive QueueBusy slices and succeed after paused producer commits");
                        drain_finished.store(true, std::sync::atomic::Ordering::Release);
                        rep
                    };
                    let ticker_fut = async move {
                        let mut interval = tokio::time::interval(Duration::from_millis(10));
                        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        // Consume the immediate t=0 first tick so every counted tick
                        // reflects genuine elapsed runtime scheduling progress.
                        interval.tick().await;
                        while !drain_finished_ticker.load(std::sync::atomic::Ordering::Acquire) {
                            interval.tick().await;
                            if wait_active_ticker.load(std::sync::atomic::Ordering::Acquire) {
                                async_ticks_task
                                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            }
                        }
                    };
                    let (rep, ()) = tokio::join!(drain_fut, ticker_fut);
                    drop(lock);
                    consumer_done_flag.store(true, std::sync::atomic::Ordering::Release);
                    (
                        rep,
                        spy.recorded_calls(),
                        obs.segment_events,
                        obs.queue_busy_retries,
                    )
                })
            });

            // Deterministically wait until consumer drained `< committed_len` (f0..f2),
            // entered `recover_lane`, AND observed >= 2 `QueueBusy` retry slices!
            entering_lock_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer must drain committed prefix and enter append.lock wait");
            let observed_retry = busy_observed_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer must deterministically observe >= 2 QueueBusy retry slices");
            assert!(observed_retry >= 2);

            // AC2 assertions while producer is STILL paused holding `append.lock`:
            assert!(
                !consumer_done.load(std::sync::atomic::Ordering::Acquire),
                "AC2 ({lane_label}): consumer must keep waiting on append.lock past single-slice deadline while producer is paused"
            );
            let first_slice_ticks =
                ticks_before_first_busy.load(std::sync::atomic::Ordering::Acquire);
            assert!(
                first_slice_ticks >= 4,
                "AC1/AC2 ({lane_label}): MissedTickBehavior::Skip 10ms timer must tick >= 4 times INSIDE the very first 80ms spawn_blocking wait before any async backoff runs (got {first_slice_ticks})"
            );
            assert!(
                is_writer_active(repo.path()).unwrap(),
                "AC1 ({lane_label}): consumer must hold writer.lock while waiting on append.lock"
            );
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: l3,
                },
                "AC2 ({lane_label}): consumer must commit strictly < committed_len before blocking"
            );
            assert_eq!(
                fs::read_dir(&init.paths.quarantine).unwrap().count(),
                0,
                "AC2 ({lane_label}): paused producer in-flight bytes must NEVER be quarantined"
            );
            assert_eq!(
                fs::metadata(&seg0_path).unwrap().len(),
                l3 + f3_mid as u64,
                "AC2 ({lane_label}): consumer must NOT truncate paused producer's in-flight bytes"
            );
            assert!(
                !seg1_path.exists(),
                "AC2 ({lane_label}): consumer must NOT skip or rotate active segment 0"
            );

            // AC3: Clear the pause-barrier flag BEFORE releasing producer (consumer is
            // blocked inside append.lock retries, NOT in any DB write), so Batch 2 is
            // guaranteed to run with `external_lock_held_a == false` and zero gap!
            external_lock_held_a.store(false, std::sync::atomic::Ordering::Release);
            prod_release_tx.send(()).expect("release paused producer");
            let prod_out = prod_handle
                .join()
                .expect("join producer thread")
                .expect("paused producer append must succeed");
            assert_eq!(prod_out.seg_id, 0);
            assert_eq!(prod_out.meta.committed_len, expected_l4);

            let (rep, calls, seg_events_a, busy_retries_a) =
                consumer_handle.join().expect("join consumer thread");
            assert!(
                busy_retries_a >= 1,
                "AC2/AC3 ({lane_label}): consumer must have retried across >= 1 QueueBusy deadline slice without aborting (got {busy_retries_a})"
            );
            assert!(rep.acquired_writer_lock);
            assert_eq!(rep.applied_frames, 4);
            assert_eq!(rep.deduplicated_frames, 0);
            assert_eq!(rep.recovery_delegations, 1);
            assert_eq!(calls.len(), 2, "batch 1 = f0..f2 (< l3), batch 2 = f3");
            assert_eq!(calls[0].item_count, 3);
            assert_eq!(calls[0].dedup_keys, expected_dedups);
            assert!(
                !calls[0].append_lock_was_free,
                "AC1 ({lane_label}): batch 1 must have committed while producer held append.lock"
            );
            assert_eq!(calls[1].item_count, 1);
            assert_eq!(calls[1].dedup_keys, vec![f3_dedup.clone()]);
            assert!(
                calls[1].append_lock_was_free,
                "AC1 ({lane_label}): OS kernel must prove append.lock is FREE during batch 2 DB write (no consumer lock leak)"
            );

            // Explicit `Phase1Classified { kind: "CaseAClean" }` observation!
            assert!(
                seg_events_a.contains(&SegmentOpEvent::Phase1Classified {
                    lane: lane_label,
                    kind: "CaseAClean",
                }),
                "AC3 ({lane_label}): Phase A recovery must explicitly classify CaseAClean; events = {seg_events_a:?}"
            );
            let recs = recovery_reports.lock().unwrap().clone();
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].recovered_frames, 0);
            assert!(
                recs[0].quarantined_artifacts.is_empty(),
                "AC2/AC3 ({lane_label}): zero LBR-AGENT-050 quarantine artifacts expected"
            );
            assert_eq!(recs[0].meta.active_seg, 0);
            assert_eq!(recs[0].meta.committed_len, expected_l4);

            // All 4 frames (incl. paused f3) persisted with EXACT payload equality!
            assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
            assert!(!seg1_path.exists());
            let hw_after_a = CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: expected_l4,
            };
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_after_a);
            assert_eq!(
                read_highwater_mirror(&init.paths.root)
                    .unwrap()
                    .unwrap()
                    .get(lane),
                hw_after_a
            );
            expected_payloads.push(f3_payload.clone());
            for expected in &expected_payloads {
                match expected {
                    CaptureBatchPayload::AppendAuditLog(rec) => {
                        let got = store
                            .get_audit_log(&rec.audit_id)
                            .await
                            .unwrap()
                            .expect("persisted audit log must exist by exact id");
                        assert_eq!(
                            &got, rec,
                            "AC3 ({lane_label}): persisted audit payload mismatch"
                        );
                    }
                    CaptureBatchPayload::UpsertSession(rec) => {
                        let got = store
                            .get_session(&rec.session_id)
                            .await
                            .unwrap()
                            .expect("persisted session must exist by exact id");
                        assert_eq!(
                            &got, rec,
                            "AC3 ({lane_label}): persisted session payload mismatch"
                        );
                    }
                    other => panic!("unexpected test payload variant: {other:?}"),
                }
            }

            // =================================================================
            // Phase B: Producer pauses AFTER full `write_all` + `sync_data()`
            // and strictly BEFORE `commit_dual_tail_meta` (f4), again beyond
            // a single `append.lock` timeout slice (160ms > 40ms).
            // =================================================================
            let f4_dedup = format!("rb09c:{lane_label}:paused-post-sync");
            let f4_payload = payload_for(&format!("rb09c-{lane_label}-sync-4"), 4);
            let f4_bytes = encode_postcard_frame(
                flags,
                &f4_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &f4_payload,
            )
            .expect("encode f4");
            let expected_l5 = expected_l4 + f4_bytes.len() as u64;

            let external_lock_held_b = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let external_lock_prod_b = Arc::clone(&external_lock_held_b);
            let (prod_paused_tx2, prod_paused_rx2) = std::sync::mpsc::channel::<()>();
            let (prod_release_tx2, prod_release_rx2) = std::sync::mpsc::channel::<()>();
            let prod_repo2 = repo.path().to_path_buf();
            let prod_f4 = f4_bytes.clone();
            let prod_handle2 = std::thread::spawn(move || {
                let mut obs = SegmentObserver::with_after_segment_sync_before_meta_hook(
                    move |_lane_name, seg_id| {
                        assert_eq!(seg_id, 0);
                        external_lock_prod_b.store(true, std::sync::atomic::Ordering::Release);
                        prod_paused_tx2
                            .send(())
                            .expect("signal producer paused post-sync");
                        prod_release_rx2
                            .recv()
                            .expect("wait for producer release 2");
                    },
                );
                append_encoded_frame_in_storage_dir_with_observer(
                    &prod_repo2,
                    lane,
                    &prod_f4,
                    QueueAppendOptions::default(),
                    &mut obs,
                )
            });

            prod_paused_rx2
                .recv_timeout(Duration::from_secs(5))
                .expect("producer must reach post-sync pre-LTM2 pause point");
            assert!(external_lock_held_b.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(fs::metadata(&seg0_path).unwrap().len(), expected_l5);
            assert_eq!(
                read_converged_lane_tail_meta(&lane_dir)
                    .unwrap()
                    .committed_len,
                expected_l4
            );

            let (entering_lock_tx2, entering_lock_rx2) = std::sync::mpsc::channel::<()>();
            let (busy_observed_tx2, busy_observed_rx2) = std::sync::mpsc::channel::<usize>();
            let consumer_done2 = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let consumer_done_flag2 = Arc::clone(&consumer_done2);
            let consumer_root2 = init.paths.root.clone();
            let consumer_store2 = store.clone();
            let consumer_ext_flag_b = Arc::clone(&external_lock_held_b);
            let consumer_handle2 = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build consumer runtime 2");
                rt.block_on(async move {
                    let lock = try_acquire_writer_lock_at_root(&consumer_root2)
                        .unwrap()
                        .unwrap();
                    let mut obs = WriterObserver::with_recovery_hooks(
                        move |_lane| {
                            entering_lock_tx2
                                .send(())
                                .expect("signal consumer entering append.lock 2");
                        },
                        move |rep| {
                            assert_eq!(rep.recovered_frames, 0);
                            assert!(rep.quarantined_artifacts.is_empty());
                        },
                    )
                    .with_append_lock_slice(Duration::from_millis(25))
                    .with_queue_busy_retry_hook(move |retry_n| {
                        if retry_n == 2 {
                            let _ = busy_observed_tx2.send(retry_n);
                        }
                    });
                    let spy = SpyCaptureStore::new_with_lock_tracking(
                        consumer_store2,
                        consumer_root2.clone(),
                        obs.consumer_holds_append_lock_handle(),
                        consumer_ext_flag_b,
                    );
                    let rep = drain_locked_at_root(
                        &consumer_root2,
                        &spy,
                        QueueWriterOptions::default(),
                        &lock,
                        &mut obs,
                    )
                    .await
                    .expect("phase B consumer drain must succeed across QueueBusy slices");
                    drop(lock);
                    consumer_done_flag2.store(true, std::sync::atomic::Ordering::Release);
                    (
                        rep,
                        spy.recorded_calls(),
                        obs.segment_events,
                        obs.queue_busy_retries,
                    )
                })
            });

            entering_lock_rx2
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer must block on append.lock in Phase B");
            let observed_retry2 = busy_observed_rx2
                .recv_timeout(Duration::from_secs(5))
                .expect("Phase B consumer must deterministically observe >= 2 QueueBusy retries");
            assert!(observed_retry2 >= 2);
            assert!(!consumer_done2.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
            assert!(!seg1_path.exists());

            external_lock_held_b.store(false, std::sync::atomic::Ordering::Release);
            prod_release_tx2
                .send(())
                .expect("release paused producer 2");
            let prod_out2 = prod_handle2.join().unwrap().unwrap();
            assert_eq!(prod_out2.meta.committed_len, expected_l5);

            let (rep2, calls_b, seg_events_b, busy_retries_b) = consumer_handle2.join().unwrap();
            assert!(
                busy_retries_b >= 1,
                "AC2/AC3 Phase B ({lane_label}): consumer must retry across QueueBusy slices (got {busy_retries_b})"
            );
            assert_eq!(rep2.applied_frames, 1);
            assert_eq!(rep2.deduplicated_frames, 0);
            assert_eq!(rep2.recovery_delegations, 1);
            assert_eq!(calls_b.len(), 1);
            assert_eq!(calls_b[0].dedup_keys, vec![f4_dedup]);
            assert!(
                calls_b[0].append_lock_was_free,
                "AC1 Phase B ({lane_label}): OS kernel must prove append.lock is FREE during DB write"
            );
            assert!(
                seg_events_b.contains(&SegmentOpEvent::Phase1Classified {
                    lane: lane_label,
                    kind: "CaseAClean",
                }),
                "AC3 Phase B ({lane_label}): recovery must explicitly classify CaseAClean; events = {seg_events_b:?}"
            );
            assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
            assert!(!seg1_path.exists());
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: expected_l5,
                }
            );
            match &f4_payload {
                CaptureBatchPayload::AppendAuditLog(rec) => {
                    let got = store
                        .get_audit_log(&rec.audit_id)
                        .await
                        .unwrap()
                        .expect("Phase B persisted audit log must exist by exact id");
                    assert_eq!(&got, rec);
                }
                CaptureBatchPayload::UpsertSession(rec) => {
                    let got = store
                        .get_session(&rec.session_id)
                        .await
                        .unwrap()
                        .expect("Phase B persisted session must exist by exact id");
                    assert_eq!(&got, rec);
                }
                other => panic!("unexpected test payload variant: {other:?}"),
            }
            assert!(!is_writer_active(repo.path()).unwrap());

            // =================================================================
            // Phase C: Cancellation safety (`writer.lock` ownership pinned across
            // in-flight `spawn_blocking` slices!). Cancelling/dropping the outer
            // drain future AND its `WriterLockGuard` while `spawn_blocking` is
            // waiting on `append.lock` MUST keep `writer.lock` held so no second
            // consumer can concurrently acquire `writer.lock` until the blocking
            // slice exits.
            // =================================================================
            let f5_dedup = format!("rb09c:{lane_label}:cancel-safety");
            let f5_payload = payload_for(&format!("rb09c-{lane_label}-cancel-5"), 5);
            let f5_bytes = encode_postcard_frame(
                flags,
                &f5_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &f5_payload,
            )
            .expect("encode f5");
            let expected_l6 = expected_l5 + f5_bytes.len() as u64;

            let (cancel_prod_paused_tx, cancel_prod_paused_rx) = std::sync::mpsc::channel::<()>();
            let (cancel_prod_release_tx, cancel_prod_release_rx) = std::sync::mpsc::channel::<()>();
            let cancel_prod_repo = repo.path().to_path_buf();
            let cancel_prod_f5 = f5_bytes.clone();
            let cancel_prod_handle = std::thread::spawn(move || {
                let mut obs = SegmentObserver::with_after_partial_frame_write_hook(
                    move |_lane_name, seg_id| {
                        assert_eq!(seg_id, 0);
                        cancel_prod_paused_tx
                            .send(())
                            .expect("signal Phase C producer paused");
                        cancel_prod_release_rx
                            .recv()
                            .expect("wait for Phase C producer release");
                    },
                );
                append_encoded_frame_in_storage_dir_with_observer(
                    &cancel_prod_repo,
                    lane,
                    &cancel_prod_f5,
                    QueueAppendOptions::default(),
                    &mut obs,
                )
            });
            cancel_prod_paused_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("Phase C producer must pause holding append.lock");

            // Start Consumer 1 on a `current_thread` runtime with a deterministic
            // barrier INSIDE the `spawn_blocking` closure: once the blocking thread
            // is actively executing, cancel and drop BOTH the drain future AND
            // `WriterLockGuard`, then hold the blocking slice at the barrier until
            // after Consumer 2's contention checks complete (zero timing reliance!).
            let (c1_cancelled_tx, c1_cancelled_rx) = std::sync::mpsc::channel::<()>();
            let (c1_release_slice_tx, c1_release_slice_rx) = std::sync::mpsc::channel::<()>();
            let c1_release_slice_rx = Arc::new(Mutex::new(c1_release_slice_rx));
            let (c1_drop_rt_tx, c1_drop_rt_rx) = std::sync::mpsc::channel::<()>();
            let c1_root = init.paths.root.clone();
            let c1_store = store.clone();
            let c1_handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build Phase C consumer runtime");
                rt.block_on(async move {
                    let lock = try_acquire_writer_lock_at_root(&c1_root)
                        .expect("c1 acquire writer.lock")
                        .expect("writer.lock free");
                    let (slice_running_tx, slice_running_rx) = tokio::sync::oneshot::channel::<()>();
                    let slice_running_tx = Mutex::new(Some(slice_running_tx));
                    let slice_barrier_rx = Arc::clone(&c1_release_slice_rx);
                    let mut obs = WriterObserver::new()
                        .with_append_lock_slice(Duration::from_millis(50))
                        .with_recovery_blocking_slice_started_hook(move || {
                            if let Some(tx) = slice_running_tx.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                            let _ = slice_barrier_rx.lock().unwrap().recv();
                        });
                    let spy = SpyCaptureStore::new(c1_store, c1_root.clone());
                    {
                        let drain_fut = drain_locked_at_root(
                            &c1_root,
                            &spy,
                            QueueWriterOptions::default(),
                            &lock,
                            &mut obs,
                        );
                        tokio::pin!(drain_fut);
                        tokio::select! {
                            _ = &mut drain_fut => {
                                panic!("Phase C: drain must not finish while blocking slice is pinned at barrier");
                            }
                            _ = slice_running_rx => {}
                        }
                        // `drain_fut` is dropped right here while the `spawn_blocking`
                        // closure is deterministically running and pinned at its barrier!
                    }
                    drop(lock);
                    c1_cancelled_tx
                        .send(())
                        .expect("signal c1 future & lock dropped");
                });
                // Wait to tear down runtime until main finishes probing contention.
                let _ = c1_drop_rt_rx.recv();
            });

            c1_cancelled_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("Phase C consumer 1 must cancel and drop outer WriterLockGuard");

            // IMMEDIATELY after Consumer 1 cancelled & dropped its `WriterLockGuard`,
            // its running `spawn_blocking` slice MUST still hold `writer.lock`,
            // blocking any contending Consumer 2!
            assert!(
                is_writer_active(repo.path()).unwrap(),
                "AC1 Phase C ({lane_label}): writer.lock must remain held while cancelled spawn_blocking slice is still running"
            );
            assert!(
                try_acquire_writer_lock(repo.path()).unwrap().is_none(),
                "AC1 Phase C ({lane_label}): contending Consumer 2 must NOT acquire writer.lock while cancelled blocking slice is running"
            );
            let contended_c2 = drain_capture_queue(repo.path(), &store)
                .await
                .expect("contended Consumer 2 must return Ok(no-op)");
            assert!(
                !contended_c2.acquired_writer_lock,
                "AC1 Phase C ({lane_label}): contending Consumer 2 must exit without acquiring writer.lock"
            );

            // Release both the pinned blocking slice and the paused producer ->
            // cancelled `spawn_blocking` finishes and drops its `Arc<File>` token
            // -> `writer.lock` releases cleanly!
            c1_release_slice_tx
                .send(())
                .expect("release Phase C blocking slice barrier");
            cancel_prod_release_tx
                .send(())
                .expect("release Phase C producer");
            let prod_out5 = cancel_prod_handle.join().unwrap().unwrap();
            assert_eq!(prod_out5.meta.committed_len, expected_l6);
            c1_drop_rt_tx.send(()).expect("allow c1 runtime drop");
            c1_handle.join().expect("join c1 thread");

            assert!(
                !is_writer_active(repo.path()).unwrap(),
                "AC1 Phase C ({lane_label}): writer.lock must release cleanly once cancelled blocking slice exits"
            );
            let c2_rep = drain_capture_queue(repo.path(), &store)
                .await
                .expect("Consumer 2 must drain f5 cleanly after cancelled slice exits");
            assert!(c2_rep.acquired_writer_lock);
            assert_eq!(c2_rep.applied_frames, 1);
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: expected_l6,
                }
            );
            match &f5_payload {
                CaptureBatchPayload::AppendAuditLog(rec) => {
                    let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                    assert_eq!(&got, rec);
                }
                CaptureBatchPayload::UpsertSession(rec) => {
                    let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                    assert_eq!(&got, rec);
                }
                other => panic!("unexpected test payload variant: {other:?}"),
            }
            assert!(!is_writer_active(repo.path()).unwrap());
        }
    }

    #[tokio::test]
    async fn consumer_segment_reclamation_coordinates_with_append_lock() {
        // `RB-09c` AC4 & AC1 (`ADR-RB-05` #4 ③, `GC-RB-04`):
        // - While a concurrent producer holds `append.lock`, measures cross-lane
        //   capacity occupancy, and pauses before committing its append to `active_seg = 1`,
        //   the concurrent consumer drains sealed `seg 0` (lock-free wrt `append.lock`),
        //   commits DB `hw = (1, 0)` (NEVER holding `append.lock` across DB writes),
        //   and blocks on `append.lock` before reclaiming `seg 0`.
        // - While the producer is paused: neither `seg 0` nor `active_seg = 1` is deleted,
        //   `LTM2` is untouched, and producer capacity accounting sees exact undisturbed bytes.
        // - Once producer finishes committing to `seg 1` and releases `append.lock`,
        //   consumer re-reads updated `LTM2` inside `append.lock`, verifies
        //   `seg == min_live_seg && seg < active_seg && seg < highwater_seg`, updates
        //   dual `LTM2` (preserving producer's newly committed `seg 1` length!) strictly
        //   BEFORE `unlink(seg 0)` + `fsync_dir`, then drains all of `seg 1` (incl. the
        //   producer's newly appended frame).
        // - Subsequent producer capacity check reflects exact reduction by `seg0_len`.
        // - Negative guard inside `append.lock`: attempting to reclaim `active_seg` or
        //   unconsumed `seg >= highwater_seg` fails closed without deleting `active_seg`.
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_040_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(tag, "2026-11-24T04:10:00Z"),
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");

            // Seed sealed `seg 0` (4 frames) + active `seg 1` (1 frame).
            let (_seg0_offsets, seg0_len, seg1_len) = seed_rotated_lane_segments(
                repo.path(),
                lane,
                &format!("rb09c-ac4-{lane_label}"),
                false,
            );
            let seg0_path = lane_dir.join(segment_log_file_name(0));
            let seg1_path = lane_dir.join(segment_log_file_name(1));
            assert!(seg0_path.exists() && seg1_path.exists());

            let s1_conc_dedup = format!("rb09c-ac4:{lane_label}:s1:concurrent");
            let s1_conc_payload = payload_for(&format!("rb09c-ac4-{lane_label}-s1-conc"), 100);
            let s1_concurrent_frame = encode_postcard_frame(
                flags,
                &s1_conc_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &s1_conc_payload,
            )
            .expect("encode concurrent s1 frame");
            let expected_seg1_after_prod = seg1_len + s1_concurrent_frame.len() as u64;

            // Spawn producer holding `append.lock`, pausing right after capacity check
            // (before writing/committing `s1_concurrent_frame` to `active_seg = 1`)
            // until the consumer deterministically observes >= 2 `QueueBusy` retry slices.
            let external_lock_held = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let external_lock_prod = Arc::clone(&external_lock_held);
            let (prod_occ_tx, prod_occ_rx) = std::sync::mpsc::channel::<CaptureQueueOccupancy>();
            let (prod_release_tx, prod_release_rx) = std::sync::mpsc::channel::<()>();
            let prod_repo = repo.path().to_path_buf();
            let prod_frame = s1_concurrent_frame.clone();
            let prod_handle = std::thread::spawn(move || {
                let mut obs = SegmentObserver::with_after_capacity_checked_hook(move |occ| {
                    external_lock_prod.store(true, std::sync::atomic::Ordering::Release);
                    prod_occ_tx
                        .send(occ)
                        .expect("send pre-reclaim capacity occupancy");
                    prod_release_rx.recv().expect("wait for producer release");
                });
                append_encoded_frame_in_storage_dir_with_observer(
                    &prod_repo,
                    lane,
                    &prod_frame,
                    QueueAppendOptions::default(),
                    &mut obs,
                )
            });

            let occ_before = prod_occ_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("producer must pause right after capacity check");
            assert!(external_lock_held.load(std::sync::atomic::Ordering::Acquire));
            let lane_occ_before = match lane {
                CaptureQueueLane::Priority => occ_before.priority_bytes,
                CaptureQueueLane::Normal => occ_before.normal_bytes,
            };
            assert_eq!(
                lane_occ_before,
                seg0_len + seg1_len,
                "AC4 ({lane_label}): producer capacity check must observe undisturbed seg 0 + seg 1 bytes"
            );

            // Run consumer concurrently on its own `current_thread` runtime:
            // - Acquires `writer.lock`
            // - Drains sealed `seg 0` and commits DB `hw = (1, 0)` while producer holds `append.lock`
            // - Reaches `call_reclaim_sealed_segment(seg 0)` and waits across QueueBusy slices!
            let (waiting_reclaim_tx, waiting_reclaim_rx) = std::sync::mpsc::channel::<u32>();
            let (reclaim_busy_tx, reclaim_busy_rx) = std::sync::mpsc::channel::<usize>();
            let consumer_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let consumer_done_flag = Arc::clone(&consumer_done);
            let consumer_root = init.paths.root.clone();
            let consumer_root_hook = init.paths.root.clone();
            let consumer_store = store.clone();
            let consumer_ext_flag = Arc::clone(&external_lock_held);

            let consumer_handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build consumer runtime");
                rt.block_on(async move {
                    let lock = try_acquire_writer_lock_at_root(&consumer_root)
                        .expect("acquire writer.lock")
                        .expect("writer.lock uncontended");
                    let mut obs = WriterObserver::with_before_reclaim_hook(
                        move |reclaim_lane, seg_id| {
                            assert_eq!(reclaim_lane, lane);
                            assert!(
                                is_writer_active_at_root(&consumer_root_hook)
                                    .expect("probe writer.lock before reclaim append.lock"),
                                "AC1/AC4: consumer must hold writer.lock before requesting append.lock for reclaim"
                            );
                            waiting_reclaim_tx
                                .send(seg_id)
                                .expect("signal consumer waiting on append.lock for reclaim");
                        },
                    )
                    .with_append_lock_slice(Duration::from_millis(25))
                    .with_queue_busy_retry_hook(move |retry_n| {
                        if retry_n == 2 {
                            let _ = reclaim_busy_tx.send(retry_n);
                        }
                    });
                    let spy = SpyCaptureStore::new_with_lock_tracking(
                        consumer_store,
                        consumer_root.clone(),
                        obs.consumer_holds_append_lock_handle(),
                        consumer_ext_flag,
                    );
                    let rep = drain_locked_at_root(
                        &consumer_root,
                        &spy,
                        QueueWriterOptions::default(),
                        &lock,
                        &mut obs,
                    )
                    .await
                    .expect(
                        "consumer drain + coordinated reclaim must succeed across QueueBusy slices",
                    );
                    drop(lock);
                    consumer_done_flag.store(true, std::sync::atomic::Ordering::Release);
                    (
                        rep,
                        obs.segment_events,
                        spy.recorded_calls(),
                        obs.queue_busy_retries,
                    )
                })
            });

            let reclaim_target_seg = waiting_reclaim_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer must drain seg 0 and reach reclaim append.lock");
            assert_eq!(reclaim_target_seg, 0);
            let reclaim_retry_seen = reclaim_busy_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer reclaim must deterministically observe >= 2 QueueBusy retries");
            assert!(reclaim_retry_seen >= 2);

            // While producer is STILL paused holding `append.lock`:
            assert!(
                !consumer_done.load(std::sync::atomic::Ordering::Acquire),
                "AC4 ({lane_label}): consumer reclaim must block on append.lock while producer holds it"
            );
            assert!(
                seg0_path.exists() && seg1_path.exists(),
                "AC4 ({lane_label}): neither seg 0 nor active seg 1 may be unlinked while producer holds append.lock"
            );
            let meta_while_paused = read_converged_lane_tail_meta(&lane_dir).unwrap();
            assert_eq!(meta_while_paused.min_live_seg, 0);
            assert_eq!(meta_while_paused.active_seg, 1);
            assert_eq!(meta_while_paused.sealed.len(), 1);
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: 0,
                },
                "AC1/AC4: seg 0 batch must already be committed to DB while producer holds append.lock"
            );

            // Clear pause-barrier flag BEFORE releasing producer (consumer is
            // blocked inside reclaim append.lock retries, NOT in any DB write),
            // so seg 1 DB commit is guaranteed to run with `external_lock_held == false`.
            external_lock_held.store(false, std::sync::atomic::Ordering::Release);
            prod_release_tx.send(()).expect("release producer");
            let prod_out = prod_handle
                .join()
                .expect("join producer")
                .expect("producer append to active seg 1 must succeed");
            assert_eq!(prod_out.seg_id, 1);
            assert_eq!(prod_out.meta.committed_len, expected_seg1_after_prod);

            // Consumer unblocks, re-reads updated LTM2 under `append.lock`, commits dual LTM2
            // (`min_live_seg = 1`, preserving `committed_len = expected_seg1_after_prod`!)
            // BEFORE `unlink(seg 0)` + `fsync_dir`, then drains ALL frames in `seg 1`!
            let (rep, seg_events, calls, busy_retries) =
                consumer_handle.join().expect("join consumer thread");
            assert!(
                busy_retries >= 1,
                "AC4 ({lane_label}): reclaim must retry cleanly across QueueBusy slices (got {busy_retries})"
            );
            assert_eq!(calls.len(), 2, "batch 0 = seg 0, batch 1 = seg 1");
            assert!(
                !calls[0].append_lock_was_free,
                "AC1/AC4 ({lane_label}): seg 0 DB batch must commit while producer holds append.lock"
            );
            assert!(
                calls[1].append_lock_was_free,
                "AC1/AC4 ({lane_label}): seg 1 DB batch must run with OS append.lock FREE"
            );
            assert!(
                calls[1].dedup_keys.contains(&s1_conc_dedup),
                "AC4 ({lane_label}): seg 1 batch must include concurrently appended frame dedup key"
            );
            assert!(rep.acquired_writer_lock);
            assert_eq!(rep.reclaimed_segments, 1);
            assert_eq!(rep.applied_frames, 6, "4 in seg 0 + 2 in seg 1");
            assert_eq!(rep.deduplicated_frames, 0);
            assert!(!seg0_path.exists(), "sealed seg 0 must be reclaimed");
            assert!(seg1_path.exists(), "active seg 1 must remain intact");
            match &s1_conc_payload {
                CaptureBatchPayload::AppendAuditLog(rec) => {
                    let got = store
                        .get_audit_log(&rec.audit_id)
                        .await
                        .unwrap()
                        .expect("concurrently appended audit record must be persisted");
                    assert_eq!(&got, rec);
                }
                CaptureBatchPayload::UpsertSession(rec) => {
                    let got = store
                        .get_session(&rec.session_id)
                        .await
                        .unwrap()
                        .expect("concurrently appended session record must be persisted");
                    assert_eq!(&got, rec);
                }
                other => panic!("unexpected test payload variant: {other:?}"),
            }

            // Verify LTM2 preserved producer's concurrent `seg 1` committed_len & updated cleanly.
            let final_meta = read_converged_lane_tail_meta(&lane_dir).unwrap();
            assert_eq!(final_meta.min_live_seg, 1);
            assert_eq!(final_meta.active_seg, 1);
            assert_eq!(final_meta.committed_len, expected_seg1_after_prod);
            assert!(final_meta.sealed.is_empty());
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: expected_seg1_after_prod,
                }
            );

            // Verify strict durability order `AcquireAppendLock < dual LTM2 < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim < ReleaseAppendLock`.
            let p_lock = seg_events
                .iter()
                .position(|e| *e == SegmentOpEvent::AcquireAppendLock)
                .expect("AcquireAppendLock");
            let p_bak_fsync = seg_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                        lane: lane_label,
                        target_name: QUEUE_TAIL_META_BAK_FILE,
                    }
                })
                .expect("FsyncLaneDirAfterMeta(tail.meta.bak)");
            let p_unlink = seg_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::ReclaimSegmentUnlink {
                        lane: lane_label,
                        seg_id: 0,
                    }
                })
                .expect("ReclaimSegmentUnlink(0)");
            let p_fsync = seg_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::FsyncLaneDirAfterReclaim {
                        lane: lane_label,
                        seg_id: 0,
                    }
                })
                .expect("FsyncLaneDirAfterReclaim(0)");
            let p_unlock = seg_events
                .iter()
                .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
                .expect("ReleaseAppendLock");
            assert!(
                p_lock < p_bak_fsync
                    && p_bak_fsync < p_unlink
                    && p_unlink < p_fsync
                    && p_fsync < p_unlock,
                "AC4 ({lane_label}): strict reclaim order violated: {seg_events:?}"
            );

            // Subsequent producer capacity measurement reflects exact reclamation of `seg 0`
            // (`expected_seg1_after_prod`, strictly reduced by `seg0_len` relative to pre-reclaim + appended frame).
            let post_frame = encode_postcard_frame(
                flags,
                &format!("rb09c-ac4:{lane_label}:s1:post-reclaim"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09c-ac4-{lane_label}-s1-post"), 101),
            )
            .expect("encode post-reclaim frame");
            let observed_occ_after = Arc::new(Mutex::new(None::<CaptureQueueOccupancy>));
            let occ_after_slot = Arc::clone(&observed_occ_after);
            let mut post_obs = SegmentObserver::with_after_capacity_checked_hook(move |occ| {
                *occ_after_slot.lock().unwrap() = Some(occ);
            });
            let post_out = append_encoded_frame_in_storage_dir_with_observer(
                repo.path(),
                lane,
                &post_frame,
                QueueAppendOptions::default(),
                &mut post_obs,
            )
            .expect("post-reclaim producer append must succeed");
            assert_eq!(post_out.seg_id, 1);
            let occ_after = observed_occ_after
                .lock()
                .unwrap()
                .expect("capacity hook must fire");
            let lane_occ_after = match lane {
                CaptureQueueLane::Priority => occ_after.priority_bytes,
                CaptureQueueLane::Normal => occ_after.normal_bytes,
            };
            assert_eq!(
                lane_occ_after, expected_seg1_after_prod,
                "AC4 ({lane_label}): capacity occupancy after reclaim must exclude reclaimed seg 0"
            );

            // Negative fail-closed guards under `append.lock`:
            // (a) Reclaiming `active_seg = 1` (even with `highwater_seg = 2`) MUST fail
            //     closed (`BackendInternal`) and NEVER delete `active_seg = 1`!
            let tree_before_bad_reclaim = snapshot_queue_tree_full(&init.paths.root);
            let bad_reclaim_err = reclaim_sealed_segment_at_root(&init.paths.root, lane, 1, 2)
                .expect_err("AC4: reclaiming active_seg under append.lock must fail closed");
            assert!(
                matches!(
                    bad_reclaim_err,
                    StorageBackendError::BackendInternal { ref detail }
                        if detail.contains("active or future")
                ),
                "AC4 ({lane_label}): expected active-segment rejection, got {bad_reclaim_err:?}"
            );
            assert!(
                seg1_path.exists(),
                "AC4 ({lane_label}): active_seg 1 must NEVER be deleted"
            );
            assert_eq!(
                snapshot_queue_tree_full(&init.paths.root),
                tree_before_bad_reclaim
            );

            // (b) Reclaiming an unconsumed segment (`seg_id == highwater_seg`) MUST also
            //     fail closed with zero disk mutation!
            let bad_hw_err = reclaim_sealed_segment_at_root(&init.paths.root, lane, 1, 1)
                .expect_err("AC4: reclaiming seg >= highwater_seg must fail closed");
            assert!(
                matches!(bad_hw_err, StorageBackendError::BackendInternal { .. }),
                "AC4 ({lane_label}): expected unconsumed-highwater rejection, got {bad_hw_err:?}"
            );
            assert!(seg1_path.exists());
            assert_eq!(
                snapshot_queue_tree_full(&init.paths.root),
                tree_before_bad_reclaim
            );
        }
    }

    #[tokio::test]
    async fn consumer_tail_recovery_delegates_case_c_without_advancing_hw() {
        // `RB-09e` AC1, AC2, AC3, AC4 & AC6 (`ADR-RB-05` #4 ②):
        // - AC1: Under `writer.lock -> capture-queue/append.lock` after verifying
        //   `[0, committed_len)`, real-crash `actual_len > committed_len` delegates
        //   to shared two-step Case C recovery: preserves `[committed_len, recovered_valid_end)`
        //   and quarantines ONLY `[recovered_valid_end, actual_len)` into `quarantine/`.
        // - AC2: Damaged tail quarantine records exact diagnostic code `LBR-AGENT-050`.
        // - AC3: When `append.lock` is released after recovery, DB `QUEUE_HIGHWATER_*`
        //   is strictly unchanged (zero premature highwater advance).
        // - AC4: When `append.lock` is released after recovery, external `highwater`
        //   mirror file is strictly byte-identical (zero premature mirror update).
        // - AC6: Consumer NEVER calls `CaptureStore::append_capture_batch` while
        //   holding `append.lock`.
        // Verified on BOTH `normal/` and `priority/` lanes across BOTH:
        // (1) Real `drain_locked_at_root` flow (probing exact state inside
        //     `on_after_append_lock_recovery` immediately after `append.lock` release
        //     and strictly before any DB batch write, then completing ingestion);
        // (2) `CaptureQueueWriter::recover_lane_uncommitted_tail` with unconsumed
        //     committed backlog `[hw_offset, committed_len)` + synced tail + half-frame,
        //     plus fail-closed rejection when `[0, committed_len)` itself is corrupt.
        use std::io::Write as _;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_050_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(tag, "2026-11-24T05:00:00Z"),
            };

            // =================================================================
            // Scenario 1: Real `drain_locked_at_root` with drained committed
            // prefix + real-crash synced frames + damaged half-frame tail.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                // Commit 2 frames and drain them so DB `hw` and external `highwater`
                // mirror both sit at `(0, l2)`.
                let mut l2 = 0u64;
                for i in 0..2u32 {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09e:s1:{lane_label}:pre:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09e-s1-{lane_label}-pre-{i}"), i64::from(i)),
                    )
                    .expect("enqueue pre-crash committed frame");
                    l2 = out.meta.committed_len;
                }
                let pre_rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("drain pre-crash frames");
                assert_eq!(pre_rep.applied_frames, 2);
                let hw_before = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: l2,
                };
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_before);
                let mirror_path = highwater_mirror_path(&init.paths.root);
                let mirror_bytes_before =
                    fs::read(&mirror_path).expect("external mirror must exist before crash");

                // Inject real producer crash: 2 complete synced frames (`f2`, `f3`)
                // followed by a truncated partial half-frame (`f4_half`), while
                // `LTM2` remains at `committed_len == l2`!
                let f2_dedup = format!("rb09e:s1:{lane_label}:tail:2");
                let f2_payload = payload_for(&format!("rb09e-s1-{lane_label}-tail-2"), 2);
                let f2_bytes = encode_postcard_frame(
                    flags,
                    &f2_dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &f2_payload,
                )
                .unwrap();

                let f3_dedup = format!("rb09e:s1:{lane_label}:tail:3");
                let f3_payload = payload_for(&format!("rb09e-s1-{lane_label}-tail-3"), 3);
                let f3_bytes = encode_postcard_frame(
                    flags,
                    &f3_dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &f3_payload,
                )
                .unwrap();

                let f4_full = encode_postcard_frame(
                    flags,
                    &format!("rb09e:s1:{lane_label}:half:4"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09e-s1-{lane_label}-half-4"), 4),
                )
                .unwrap();
                let damaged_tail = f4_full[..f4_full.len() / 2].to_vec();
                let valid_end = l2 + f2_bytes.len() as u64 + f3_bytes.len() as u64;

                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .expect("open seg 0 for simulated crash tail");
                    f.write_all(&f2_bytes).unwrap();
                    f.write_all(&f3_bytes).unwrap();
                    f.write_all(&damaged_tail).unwrap();
                    f.sync_data().unwrap();
                }

                // Run real `drain_locked_at_root` and verify AC1–AC4 + AC6 at the
                // exact moment `append.lock` is released (strictly before any DB batch write!).
                let tx_before = store.write_transaction_count();
                let verified_at_recovery = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let verified_hook = Arc::clone(&verified_at_recovery);
                let store_hook = store.clone();
                let tracked_calls = Arc::new(Mutex::new(Vec::<RecordedBatchCall>::new()));
                let spy_calls_hook = Arc::clone(&tracked_calls);
                let mirror_path_hook = mirror_path.clone();
                let mirror_bytes_hook = mirror_bytes_before.clone();
                let damaged_tail_hook = damaged_tail.clone();
                let quarantine_dir_hook = init.paths.quarantine.clone();

                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut obs = WriterObserver::with_recovery_hooks(
                    move |rec_lane| {
                        assert_eq!(rec_lane, lane);
                    },
                    move |rec| {
                        // AC1: preserved `[l2, valid_end)` (2 frames) and quarantined ONLY `[valid_end, actual_len)` + sealed `seg 0` -> `seg 1`.
                        assert_eq!(rec.recovered_frames, 2);
                        assert_eq!(rec.recovered_bytes, valid_end - l2);
                        assert_eq!(rec.meta.active_seg, 1);
                        assert_eq!(rec.meta.sealed.len(), 1);
                        assert_eq!(rec.meta.sealed[0].seg_id, 0);
                        assert_eq!(rec.meta.sealed[0].sealed_len, valid_end);

                        // AC2: exact diagnostic code `LBR-AGENT-050` + exact damaged bytes in `quarantine/`.
                        assert_eq!(rec.quarantined_artifacts.len(), 1);
                        let qa = &rec.quarantined_artifacts[0];
                        assert_eq!(
                            qa.diagnostic_code, QUARANTINE_DIAGNOSTIC_CODE,
                            "AC2 ({lane_label}): expected LBR-AGENT-050"
                        );
                        assert_eq!(qa.diagnostic_code, "LBR-AGENT-050");
                        assert!(
                            qa.bin_path.starts_with(&quarantine_dir_hook),
                            "AC1 ({lane_label}): quarantine artifact must live under quarantine/"
                        );
                        let q_bytes =
                            fs::read(&qa.bin_path).expect("read quarantined damaged tail artifact");
                        assert_eq!(
                            q_bytes, damaged_tail_hook,
                            "AC1 ({lane_label}): quarantine artifact must hold ONLY [recovered_valid_end, actual_len)"
                        );

                        // AC3 & AC6: 0 DB writes and 0 `append_capture_batch` calls on actual passed store under `append.lock`!
                        assert_eq!(
                            store_hook.write_transaction_count(),
                            tx_before,
                            "AC3/AC6 ({lane_label}): zero DB write tx allowed during append.lock recovery"
                        );
                        assert_eq!(
                            spy_calls_hook.lock().unwrap().len(),
                            0,
                            "AC6 ({lane_label}): append_capture_batch must NEVER be called during append.lock recovery"
                        );

                        // AC4: external `highwater` mirror file strictly byte-identical on disk!
                        assert_eq!(
                            fs::read(&mirror_path_hook).unwrap(),
                            mirror_bytes_hook,
                            "AC4 ({lane_label}): external highwater mirror must be unchanged on append.lock release"
                        );

                        verified_hook.store(true, std::sync::atomic::Ordering::Release);
                    },
                );
                let mut spy_tracked = SpyCaptureStore::new_with_lock_tracking(
                    store.clone(),
                    init.paths.root.clone(),
                    obs.consumer_holds_append_lock_handle(),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                );
                spy_tracked.calls = tracked_calls;
                let rep = drain_locked_at_root(
                    &init.paths.root,
                    &spy_tracked,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut obs,
                )
                .await
                .expect("drain after Case C tail recovery must succeed");
                drop(lock);

                assert!(
                    verified_at_recovery.load(std::sync::atomic::Ordering::Acquire),
                    "recovery hook must have fired and verified AC1–AC4 & AC6"
                );
                assert!(
                    obs.segment_events
                        .contains(&SegmentOpEvent::Phase1Classified {
                            lane: lane_label,
                            kind: "CaseCRecoverTail",
                        }),
                    "AC1 ({lane_label}): recovery must classify CaseCRecoverTail"
                );
                assert_eq!(rep.recovery_delegations, 1);
                assert_eq!(rep.applied_frames, 2);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);

                let post_calls = spy_tracked.recorded_calls();
                assert_eq!(post_calls.len(), 1);
                assert!(
                    post_calls[0].append_lock_was_free,
                    "AC6 ({lane_label}): DB write after recovery must run with append.lock FREE"
                );
                assert_eq!(post_calls[0].dedup_keys, vec![f2_dedup, f3_dedup]);
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                for expected in [&f2_payload, &f3_payload] {
                    match expected {
                        CaptureBatchPayload::AppendAuditLog(rec) => {
                            let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        CaptureBatchPayload::UpsertSession(rec) => {
                            let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        other => panic!("unexpected payload: {other:?}"),
                    }
                }
            }

            // =================================================================
            // Scenario 2: `CaptureQueueWriter::recover_lane_uncommitted_tail`
            // with unconsumed committed backlog `[hw_offset, committed_len)` +
            // synced tail + damaged half-frame (direct AC1–AC4 & AC6 proof
            // with explicit DB highwater query right after `append.lock` release),
            // plus fail-closed guard when `[0, committed_len)` is corrupt.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                bootstrap_writer_lock(&init.paths.root);
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                // Drain 1 frame -> DB `hw` and mirror sit at `(0, l1)`.
                let f0_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09e:s2:{lane_label}:0"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09e-s2-{lane_label}-0"), 0),
                )
                .unwrap();
                let l1 = f0_out.meta.committed_len;
                let _ = drain_capture_queue(repo.path(), &spy).await.unwrap();
                let hw_before = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: l1,
                };
                let mirror_path = highwater_mirror_path(&init.paths.root);
                let mirror_bytes_before = fs::read(&mirror_path).unwrap();

                // Append 2 committed backlog frames (`[l1, l3)`), NOT drained yet!
                let mut l3 = l1;
                for i in 1..=2u32 {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09e:s2:{lane_label}:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09e-s2-{lane_label}-{i}"), i64::from(i)),
                    )
                    .unwrap();
                    l3 = out.meta.committed_len;
                }

                // Append 1 valid synced frame + 1 damaged half-frame after `l3`.
                let f3_bytes = encode_postcard_frame(
                    flags,
                    &format!("rb09e:s2:{lane_label}:tail:3"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09e-s2-{lane_label}-tail-3"), 3),
                )
                .unwrap();
                let damaged = b"RB09E_PARTIAL_CORRUPT_TAIL".to_vec();
                let valid_end = l3 + f3_bytes.len() as u64;
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .unwrap();
                    f.write_all(&f3_bytes).unwrap();
                    f.write_all(&damaged).unwrap();
                    f.sync_data().unwrap();
                }

                // Negative guard: if committed prefix `[0, l3)` is bitflipped,
                // `recover_lane_uncommitted_tail` MUST fail closed BEFORE acquiring
                // `append.lock` or quarantining anything!
                let saved_seg0 = fs::read(&seg0_path).unwrap();
                let mut bad_prefix = saved_seg0.clone();
                bad_prefix[(l1 / 2) as usize] ^= 0x80;
                write_bytes_0600(&seg0_path, &bad_prefix);
                let tree_before_bad = snapshot_queue_tree_full(&init.paths.root);
                let bad_err = CaptureQueueWriter::in_storage_dir(repo.path())
                    .recover_lane_uncommitted_tail(lane, &spy)
                    .await
                    .expect_err("corrupt committed prefix must fail closed before Case C recovery");
                assert_corrupt_committed_frame(bad_err, "RB-09e corrupt committed prefix guard");
                assert_eq!(snapshot_queue_tree_full(&init.paths.root), tree_before_bad);
                assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
                write_bytes_0600(&seg0_path, &saved_seg0);

                // Now invoke real `recover_lane_uncommitted_tail`:
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let rec = CaptureQueueWriter::in_storage_dir(repo.path())
                    .recover_lane_uncommitted_tail(lane, &spy)
                    .await
                    .expect("recover_lane_uncommitted_tail must succeed")
                    .expect("must return Some(LaneRecoveryReport)");

                // AC1 & AC2:
                assert_eq!(rec.recovered_frames, 1);
                assert_eq!(rec.recovered_bytes, f3_bytes.len() as u64);
                assert_eq!(rec.quarantined_artifacts.len(), 1);
                assert_eq!(
                    rec.quarantined_artifacts[0].diagnostic_code,
                    QUARANTINE_DIAGNOSTIC_CODE
                );
                assert!(
                    rec.quarantined_artifacts[0]
                        .bin_path
                        .starts_with(&init.paths.quarantine)
                );
                assert_eq!(
                    fs::read(&rec.quarantined_artifacts[0].bin_path).unwrap(),
                    damaged
                );
                assert_eq!(rec.meta.active_seg, 1);
                assert_eq!(rec.meta.sealed[0].sealed_len, valid_end);

                // AC3: DB `QUEUE_HIGHWATER_*` strictly unchanged at `(0, l1)`!
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    hw_before,
                    "AC3 ({lane_label}): DB highwater must remain completely unchanged after recovery"
                );
                // AC4: external `highwater` mirror strictly byte-identical!
                assert_eq!(
                    fs::read(&mirror_path).unwrap(),
                    mirror_bytes_before,
                    "AC4 ({lane_label}): external highwater mirror must remain byte-identical after recovery"
                );
                // AC6: zero DB write transactions & zero `append_capture_batch` calls!
                assert_eq!(store.write_transaction_count(), tx_before);
                assert_eq!(spy.recorded_calls().len(), calls_before);
                assert!(probe_append_lock_is_free(&init.paths.root));
                assert!(!is_writer_active(repo.path()).unwrap());
            }
        }
    }

    #[tokio::test]
    async fn consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock() {
        // `RB-09e` AC5 & AC6 (`ADR-RB-05` #4 ②):
        // - AC5: When `actual_len > committed_len` consists 100% of complete
        //   valid CRC32C frames (`recovered_valid_end == actual_len`), consumer
        //   recovery under `writer.lock -> append.lock` advances `LTM2`
        //   `committed_len` in place (keeping `active_seg == S`), leaves
        //   `quarantine/` 100% empty, and emits ZERO `LBR-AGENT-050`.
        // - AC6 (+ AC3/AC4): Zero `CaptureStore::append_capture_batch` calls,
        //   zero DB `QUEUE_HIGHWATER_*` changes, and zero external `highwater`
        //   mirror mutations occur while holding `append.lock`.
        // Verified on BOTH `normal/` and `priority/` lanes using REAL producer
        // `SegmentCrashPoint::AfterSegmentSyncData` crash injection!
        use std::io::Write as _;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_060_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(tag, "2026-11-24T05:10:00Z"),
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");
            let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

            // Drain initial frame `f0` -> DB `hw` & external mirror sit at `(0, l1)`.
            let f0_out = append_postcard_frame(
                repo.path(),
                lane,
                flags,
                &format!("rb09e:ac5:{lane_label}:0"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09e-ac5-{lane_label}-0"), 0),
            )
            .unwrap();
            let l1 = f0_out.meta.committed_len;
            let _ = drain_capture_queue(repo.path(), &spy).await.unwrap();
            let mirror_path = highwater_mirror_path(&init.paths.root);
            let mirror_bytes_before = fs::read(&mirror_path).unwrap();

            // Inject genuine producer crash right after `seg.sync_data()` and
            // before `LTM2` commit (`SegmentCrashPoint::AfterSegmentSyncData`) for `f1`,
            // plus a second complete synced frame `f2` (zero damaged bytes!).
            let f1_dedup = format!("rb09e:ac5:{lane_label}:synced:1");
            let f1_payload = payload_for(&format!("rb09e-ac5-{lane_label}-synced-1"), 1);
            let f1_bytes = encode_postcard_frame(
                flags,
                &f1_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &f1_payload,
            )
            .unwrap();
            let mut crash_prod_obs =
                SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
            let _ = append_encoded_frame_in_storage_dir_with_observer(
                repo.path(),
                lane,
                &f1_bytes,
                QueueAppendOptions::default(),
                &mut crash_prod_obs,
            )
            .expect_err("simulated AfterSegmentSyncData producer crash must fail");

            let f2_dedup = format!("rb09e:ac5:{lane_label}:synced:2");
            let f2_payload = payload_for(&format!("rb09e-ac5-{lane_label}-synced-2"), 2);
            let f2_bytes = encode_postcard_frame(
                flags,
                &f2_dedup,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &f2_payload,
            )
            .unwrap();
            let seg0_path = lane_dir.join(segment_log_file_name(0));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&f2_bytes).unwrap();
                f.sync_data().unwrap();
            }
            let expected_l3 = l1 + f1_bytes.len() as u64 + f2_bytes.len() as u64;
            assert_eq!(fs::metadata(&seg0_path).unwrap().len(), expected_l3);
            assert_eq!(
                read_converged_lane_tail_meta(&lane_dir)
                    .unwrap()
                    .committed_len,
                l1
            );

            // Part 1: Run real `drain_locked_at_root` and verify AC5 & AC6 right
            // upon `append.lock` release (strictly before any DB batch commits!).
            let tx_before = store.write_transaction_count();
            let verified_pure = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let verified_pure_hook = Arc::clone(&verified_pure);
            let store_hook = store.clone();
            let tracked_calls_p1 = Arc::new(Mutex::new(Vec::<RecordedBatchCall>::new()));
            let spy_calls_hook_p1 = Arc::clone(&tracked_calls_p1);
            let mirror_path_hook = mirror_path.clone();
            let mirror_bytes_hook = mirror_bytes_before.clone();
            let quarantine_dir_hook = init.paths.quarantine.clone();

            let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                .unwrap()
                .unwrap();
            let mut obs = WriterObserver::with_recovery_hooks(
                move |rec_lane| {
                    assert_eq!(rec_lane, lane);
                },
                move |rec| {
                    // AC5: `LTM2.committed_len` advanced in place to `expected_l3`,
                    // active segment kept at `0`, ZERO quarantine artifacts, ZERO LBR-AGENT-050!
                    assert_eq!(rec.recovered_frames, 2);
                    assert_eq!(rec.recovered_bytes, expected_l3 - l1);
                    assert_eq!(rec.meta.active_seg, 0);
                    assert_eq!(rec.meta.committed_len, expected_l3);
                    assert!(rec.meta.sealed.is_empty());
                    assert!(
                        rec.quarantined_artifacts.is_empty(),
                        "AC5 ({lane_label}): pure synced tail must emit zero LBR-AGENT-050 artifacts"
                    );
                    assert_eq!(
                        fs::read_dir(&quarantine_dir_hook).unwrap().count(),
                        0,
                        "AC5 ({lane_label}): quarantine/ must remain 100% empty"
                    );
                    // AC6 (+ AC3/AC4): zero DB write tx, zero `append_capture_batch` calls on actual passed store, & external mirror byte-identical!
                    assert_eq!(
                        store_hook.write_transaction_count(),
                        tx_before,
                        "AC6 ({lane_label}): zero DB write tx permitted during append.lock recovery"
                    );
                    assert_eq!(
                        spy_calls_hook_p1.lock().unwrap().len(),
                        0,
                        "AC6 ({lane_label}): append_capture_batch must NEVER be called during append.lock recovery"
                    );
                    assert_eq!(
                        fs::read(&mirror_path_hook).unwrap(),
                        mirror_bytes_hook,
                        "AC4/AC5 ({lane_label}): external highwater mirror must be unchanged on lock release"
                    );
                    verified_pure_hook.store(true, std::sync::atomic::Ordering::Release);
                },
            );
            let mut spy_tracked = SpyCaptureStore::new_with_lock_tracking(
                store.clone(),
                init.paths.root.clone(),
                obs.consumer_holds_append_lock_handle(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
            spy_tracked.calls = tracked_calls_p1;
            let rep = drain_locked_at_root(
                &init.paths.root,
                &spy_tracked,
                QueueWriterOptions::default(),
                &lock,
                &mut obs,
            )
            .await
            .expect("drain after pure synced tail recovery must succeed");
            drop(lock);

            assert!(verified_pure.load(std::sync::atomic::Ordering::Acquire));
            assert!(
                obs.segment_events
                    .contains(&SegmentOpEvent::Phase1Classified {
                        lane: lane_label,
                        kind: "CaseCRecoverTail",
                    })
            );
            assert_eq!(rep.recovery_delegations, 1);
            assert_eq!(rep.applied_frames, 2);
            assert_eq!(rep.deduplicated_frames, 0);
            assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
            assert!(!lane_dir.join(segment_log_file_name(1)).exists());

            let calls = spy_tracked.recorded_calls();
            assert_eq!(calls.len(), 1);
            assert!(
                calls[0].append_lock_was_free,
                "AC6 ({lane_label}): DB write after pure-tail recovery must run with append.lock FREE"
            );
            assert_eq!(calls[0].dedup_keys, vec![f1_dedup, f2_dedup]);
            let hw_after_p1 = CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: expected_l3,
            };
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_after_p1);
            for expected in [&f1_payload, &f2_payload] {
                match expected {
                    CaptureBatchPayload::AppendAuditLog(rec) => {
                        let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                        assert_eq!(&got, rec);
                    }
                    CaptureBatchPayload::UpsertSession(rec) => {
                        let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                        assert_eq!(&got, rec);
                    }
                    other => panic!("unexpected payload: {other:?}"),
                }
            }

            // Part 2: Direct `CaptureQueueWriter::recover_lane_uncommitted_tail` on
            // another `AfterSegmentSyncData` crash confirms zero DB tx, zero HW
            // change, zero mirror change, and zero quarantine before drain runs.
            let mirror_bytes_p2 = fs::read(&mirror_path).unwrap();
            let f3_bytes = encode_postcard_frame(
                flags,
                &format!("rb09e:ac5:{lane_label}:synced:3"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09e-ac5-{lane_label}-synced-3"), 3),
            )
            .unwrap();
            let mut crash_prod_obs2 =
                SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
            let _ = append_encoded_frame_in_storage_dir_with_observer(
                repo.path(),
                lane,
                &f3_bytes,
                QueueAppendOptions::default(),
                &mut crash_prod_obs2,
            )
            .expect_err("simulated second AfterSegmentSyncData crash must fail");
            let expected_l4 = expected_l3 + f3_bytes.len() as u64;

            let tx_before_p2 = store.write_transaction_count();
            let calls_before_p2 = spy.recorded_calls().len();
            let rec2 = CaptureQueueWriter::in_storage_dir(repo.path())
                .recover_lane_uncommitted_tail(lane, &spy)
                .await
                .expect("direct pure-tail recovery must succeed")
                .expect("must recover pure synced frame f3");
            assert_eq!(rec2.recovered_frames, 1);
            assert_eq!(rec2.meta.active_seg, 0);
            assert_eq!(rec2.meta.committed_len, expected_l4);
            assert!(rec2.quarantined_artifacts.is_empty());
            assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
            assert_eq!(store.write_transaction_count(), tx_before_p2);
            assert_eq!(spy.recorded_calls().len(), calls_before_p2);
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_after_p1);
            assert_eq!(fs::read(&mirror_path).unwrap(), mirror_bytes_p2);
            assert!(probe_append_lock_is_free(&init.paths.root));
            assert!(!is_writer_active(repo.path()).unwrap());
        }
    }

    #[tokio::test]
    async fn consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance() {
        // `RB-09f` AC1–AC5 (`ADR-RB-05` #4 ② (b), ③):
        // Verify on BOTH `normal/` and `priority/` lanes:
        // - Construct segment `S = 0` with:
        //   * 5 pre-consumed frames `[0, hw_offset)` (so `hw_offset > 0`),
        //   * 135 (> 128) unconsumed committed backlog frames `[hw_offset, committed_len)`,
        //   * 135 (> 128) complete `sync_data` uncommitted tail frames
        //     `[committed_len, sealed_len(0))`,
        //   * plus a damaged half-frame tail `[sealed_len(0), actual_len)`.
        // - Run `RB-09e` `recover_lane_uncommitted_tail` (Step C2 seals `S = 0`
        //   at `sealed_len(0)` and switches `active_seg = 1`, quarantining ONLY
        //   the half-frame with `LBR-AGENT-050`, leaving DB highwater untouched
        //   at `(0, hw_offset)`).
        // - Run normal consumer drain across `S = 0` (270 frames => >= 3 batches):
        //   * AC1: 100% of the 135 backlog frames persisted in `capture.redb`
        //     with full field-for-field payload equality.
        //   * AC2: 100% of the 135 recovered synced tail frames persisted in
        //     `capture.redb` with full field-for-field payload equality.
        //   * AC3: Every intermediate batch (`batch_end_offset < sealed_len(0)`)
        //     commits 1 single redb `WriteTransaction` advancing authoritative DB
        //     `QUEUE_HIGHWATER_*` strictly to `(0, batch_end_offset)` — NEVER
        //     jumping prematurely to `(1, 0)` and NEVER reclaiming `seg 0` early.
        //   * AC4: ONLY the final batch reaching `batch_end_offset == sealed_len(0)`
        //     atomically advances DB `QUEUE_HIGHWATER_*` to `(1, 0)` inside that
        //     exact same `WriteTransaction`.
        //   * AC5: Strictly after that final DB commit advances highwater past
        //     `S = 0`, consumer reclaims `events-00000000.log` in strict order:
        //     `AcquireAppendLock < MetaRename(tail.meta) < FsyncLaneDirAfterMeta(tail.meta)
        //      < MetaRename(tail.meta.bak) < FsyncLaneDirAfterMeta(tail.meta.bak)
        //      < ReclaimSegmentUnlink(0) < FsyncLaneDirAfterReclaim(0) < ReleaseAppendLock`.
        use std::io::Write as _;

        const PRE_COUNT: usize = 5;
        const BACKLOG_COUNT: usize = 135;
        const TAIL_COUNT: usize = 135;
        const TOTAL_RECOVERED_AND_BACKLOG: usize = BACKLOG_COUNT + TAIL_COUNT;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_070_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(
                    tag,
                    &format!("2026-11-24T06:{:02}:{:02}Z", (seq / 60) % 60, seq % 60),
                ),
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");
            let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

            // 1. Seed & drain `PRE_COUNT` frames so DB `hw` sits at `(0, hw_offset)` with `hw_offset > 0`.
            let mut hw_offset = 0u64;
            for i in 0..PRE_COUNT {
                let out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09f:{lane_label}:pre:{i:03}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09f-{lane_label}-pre-{i:03}"), i as i64),
                )
                .expect("enqueue pre-consumed frame");
                hw_offset = out.meta.committed_len;
            }
            let pre_rep = drain_capture_queue(repo.path(), &setup_spy)
                .await
                .expect("drain pre-consumed frames");
            assert_eq!(pre_rep.applied_frames, PRE_COUNT);
            let hw_before = CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: hw_offset,
            };
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_before);
            assert!(hw_offset > 0);

            // 2. Append `BACKLOG_COUNT` (135 > 128) committed unconsumed backlog frames `[hw_offset, committed_len)`.
            let mut expected_dedups: Vec<String> = Vec::with_capacity(TOTAL_RECOVERED_AND_BACKLOG);
            let mut expected_payloads: Vec<CaptureBatchPayload> =
                Vec::with_capacity(TOTAL_RECOVERED_AND_BACKLOG);
            let mut frame_end_offsets: Vec<u64> = Vec::with_capacity(TOTAL_RECOVERED_AND_BACKLOG);
            let mut cursor = hw_offset;
            for i in 0..BACKLOG_COUNT {
                let dedup = format!("rb09f:{lane_label}:backlog:{i:03}");
                let payload = payload_for(
                    &format!("rb09f-{lane_label}-backlog-{i:03}"),
                    (PRE_COUNT + i) as i64,
                );
                let out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload,
                )
                .expect("enqueue backlog frame");
                assert_eq!(out.frame_offset, cursor);
                cursor = out.meta.committed_len;
                expected_dedups.push(dedup);
                expected_payloads.push(payload);
                frame_end_offsets.push(cursor);
            }
            let committed_len_before = cursor;

            // 3. Append `TAIL_COUNT` (135 > 128) complete `sync_data` valid frames
            //    followed by 1 truncated half-frame directly to `events-00000000.log`
            //    without updating `LTM2`.
            let mut raw_tail_buf = Vec::new();
            for i in 0..TAIL_COUNT {
                let dedup = format!("rb09f:{lane_label}:tail:{i:03}");
                let payload = payload_for(
                    &format!("rb09f-{lane_label}-tail-{i:03}"),
                    (PRE_COUNT + BACKLOG_COUNT + i) as i64,
                );
                let encoded = encode_postcard_frame(
                    flags,
                    &dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload,
                )
                .expect("encode synced tail frame");
                cursor += encoded.len() as u64;
                raw_tail_buf.extend_from_slice(&encoded);
                expected_dedups.push(dedup);
                expected_payloads.push(payload);
                frame_end_offsets.push(cursor);
            }
            let expected_sealed_len = cursor;

            let half_frame_full = encode_postcard_frame(
                flags,
                &format!("rb09f:{lane_label}:half"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09f-{lane_label}-half"), 9999),
            )
            .expect("encode half frame");
            let damaged_tail = half_frame_full[..half_frame_full.len() / 2].to_vec();

            let seg0_path = lane_dir.join(segment_log_file_name(0));
            let seg1_path = lane_dir.join(segment_log_file_name(1));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .expect("open seg 0 to append synced tail + damaged half-frame");
                f.write_all(&raw_tail_buf).unwrap();
                f.write_all(&damaged_tail).unwrap();
                f.sync_data().unwrap();
            }
            assert_eq!(
                read_converged_lane_tail_meta(&lane_dir)
                    .unwrap()
                    .committed_len,
                committed_len_before
            );

            // 4. Delegate Case C Step C2 tail recovery via `RB-09e` without advancing DB highwater.
            let rec = CaptureQueueWriter::in_storage_dir(repo.path())
                .recover_lane_uncommitted_tail(lane, &setup_spy)
                .await
                .expect("RB-09e recovery delegation must succeed")
                .expect("must recover synced tail + quarantine damaged half-frame");
            assert_eq!(rec.recovered_frames, TAIL_COUNT);
            assert_eq!(
                rec.recovered_bytes,
                expected_sealed_len - committed_len_before
            );
            assert_eq!(rec.meta.min_live_seg, 0);
            assert_eq!(rec.meta.active_seg, 1);
            assert_eq!(rec.meta.committed_len, 0);
            assert_eq!(rec.meta.sealed.len(), 1);
            assert_eq!(rec.meta.sealed[0].seg_id, 0);
            assert_eq!(rec.meta.sealed[0].sealed_len, expected_sealed_len);
            assert_eq!(rec.quarantined_artifacts.len(), 1);
            assert_eq!(
                rec.quarantined_artifacts[0].diagnostic_code,
                QUARANTINE_DIAGNOSTIC_CODE
            );
            assert_eq!(
                fs::read(&rec.quarantined_artifacts[0].bin_path).unwrap(),
                damaged_tail
            );
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                hw_before,
                "RB-09e recovery must leave DB highwater at (0, hw_offset) before normal drain"
            );

            // 5. Run normal consumer drain across sealed `seg 0` and capture exact
            //    per-commit DB highwater + on-disk reclamation timing right after
            //    every `CaptureStore::append_capture_batch` transaction!
            #[derive(Debug, Clone)]
            struct BatchCommitSnapshot {
                seg0_existed_right_after_tx: bool,
                min_live_seg_right_after_tx: u32,
                sealed_count_right_after_tx: usize,
            }

            let commit_snapshots = Arc::new(Mutex::new(Vec::<BatchCommitSnapshot>::new()));
            let snap_hook = Arc::clone(&commit_snapshots);
            let seg0_hook = seg0_path.clone();
            let lane_dir_hook = lane_dir.clone();

            let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                .expect("acquire writer.lock")
                .expect("writer.lock free");
            let mut obs = WriterObserver::new();
            let spy = SpyCaptureStore::new_with_lock_tracking(
                store.clone(),
                init.paths.root.clone(),
                obs.consumer_holds_append_lock_handle(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .with_after_batch_callback(move |_call| {
                let meta_now = read_converged_lane_tail_meta(&lane_dir_hook)
                    .expect("LTM2 readable right after DB tx");
                snap_hook.lock().unwrap().push(BatchCommitSnapshot {
                    seg0_existed_right_after_tx: seg0_hook.exists(),
                    min_live_seg_right_after_tx: meta_now.min_live_seg,
                    sealed_count_right_after_tx: meta_now.sealed.len(),
                });
            });

            let tx_before_drain = store.write_transaction_count();
            let rep = drain_locked_at_root(
                &init.paths.root,
                &spy,
                QueueWriterOptions::default(),
                &lock,
                &mut obs,
            )
            .await
            .expect("normal multi-batch drain across Step C2 sealed seg 0 must succeed");
            let tx_after_drain = store.write_transaction_count();
            drop(lock);

            assert!(rep.acquired_writer_lock);
            assert_eq!(rep.applied_frames, TOTAL_RECOVERED_AND_BACKLOG);
            assert_eq!(rep.deduplicated_frames, 0);
            assert_eq!(rep.reclaimed_segments, 1);
            assert_eq!(rep.recovery_delegations, 0);

            let calls = spy.recorded_calls();
            let snaps = commit_snapshots.lock().unwrap().clone();
            assert_eq!(calls.len(), snaps.len());
            assert!(
                calls.len() >= 3,
                "AC3 ({lane_label}): 270 frames at max_batch_size=128 must require >= 3 batches, got {}",
                calls.len()
            );
            assert_eq!(
                tx_after_drain - tx_before_drain,
                calls.len() as u64,
                "each batch must commit in exactly 1 redb WriteTransaction"
            );

            // AC3 & AC4: verify EVERY intermediate batch stays at `(0, batch_end_offset)`
            // without jumping to `(1, 0)` or reclaiming early, and ONLY the final
            // batch exhausting `sealed_len(0)` atomically advances to `(1, 0)`.
            let mut cumulative_frames = 0usize;
            let mut prev_offset = hw_offset;
            let mut all_seen_dedups = Vec::with_capacity(TOTAL_RECOVERED_AND_BACKLOG);
            for (idx, (call, snap)) in calls.iter().zip(snaps.iter()).enumerate() {
                assert_eq!(call.lane, lane);
                assert!(call.append_lock_was_free);
                assert_eq!(call.tx_delta, 1);
                assert!(
                    (1..=DEFAULT_WRITER_MAX_BATCH_SIZE).contains(&call.item_count),
                    "batch {idx} item_count out of bounds: {}",
                    call.item_count
                );
                assert_eq!(call.highwater, call.db_highwater_after);
                all_seen_dedups.extend(call.dedup_keys.iter().cloned());
                cumulative_frames += call.item_count;
                let batch_end_offset = frame_end_offsets[cumulative_frames - 1];

                // Right at DB commit time (before post-commit reclaim), seg 0 must still be live!
                assert!(
                    snap.seg0_existed_right_after_tx,
                    "AC3/AC5 ({lane_label}): batch {idx} must NOT unlink seg 0 before final DB commit finishes"
                );
                assert_eq!(
                    snap.min_live_seg_right_after_tx, 0,
                    "AC3/AC5 ({lane_label}): LTM2 min_live_seg must still be 0 at DB tx commit time for batch {idx}"
                );
                assert_eq!(snap.sealed_count_right_after_tx, 1);

                if idx + 1 < calls.len() {
                    // AC3: strict intermediate batch assertion.
                    assert!(
                        batch_end_offset > prev_offset && batch_end_offset < expected_sealed_len,
                        "AC3 ({lane_label}): intermediate batch {idx} end offset {batch_end_offset} must lie strictly inside ({prev_offset}, {expected_sealed_len})"
                    );
                    assert_eq!(
                        call.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: 0,
                            byte_offset: batch_end_offset,
                        },
                        "AC3 ({lane_label}): intermediate batch {idx} must advance strictly to (0, batch_end_offset), NEVER (1, 0)"
                    );
                    prev_offset = batch_end_offset;
                } else {
                    // AC4: final batch reaching `sealed_len(0)` atomically jumps to `(1, 0)`.
                    assert_eq!(cumulative_frames, TOTAL_RECOVERED_AND_BACKLOG);
                    assert!(
                        prev_offset < expected_sealed_len,
                        "AC4 ({lane_label}): prior batch must not have already reached sealed_len(0)"
                    );
                    assert_eq!(
                        batch_end_offset, expected_sealed_len,
                        "AC4 ({lane_label}): final batch must terminate at exact sealed_len(0)"
                    );
                    assert_eq!(
                        call.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: 1,
                            byte_offset: 0,
                        },
                        "AC4 ({lane_label}): final batch exhausting sealed_len(0) must atomically advance DB highwater to (1, 0)"
                    );
                }
            }
            assert_eq!(all_seen_dedups, expected_dedups);

            // AC1 & AC2: 100% of backlog frames `[hw_offset, committed_len)` AND
            // 100% of recovered synced tail frames `[committed_len, sealed_len(0))`
            // persisted in `capture.redb` with exact payload equality!
            for (idx, expected) in expected_payloads.iter().enumerate() {
                match expected {
                    CaptureBatchPayload::AppendAuditLog(rec) => {
                        let got = store
                            .get_audit_log(&rec.audit_id)
                            .await
                            .unwrap()
                            .unwrap_or_else(|| {
                                panic!("AC1/AC2 ({lane_label}): missing persisted audit frame #{idx} ({})", rec.audit_id)
                            });
                        assert_eq!(&got, rec);
                    }
                    CaptureBatchPayload::UpsertSession(rec) => {
                        let got = store
                            .get_session(&rec.session_id)
                            .await
                            .unwrap()
                            .unwrap_or_else(|| {
                                panic!("AC1/AC2 ({lane_label}): missing persisted session frame #{idx} ({})", rec.session_id)
                            });
                        assert_eq!(&got, rec);
                    }
                    other => panic!("unexpected payload: {other:?}"),
                }
            }
            match lane {
                CaptureQueueLane::Normal => {
                    assert_eq!(
                        store
                            .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                            .await
                            .unwrap()
                            .len(),
                        PRE_COUNT + TOTAL_RECOVERED_AND_BACKLOG
                    );
                }
                CaptureQueueLane::Priority => {
                    assert_eq!(
                        store.list_sessions_by_started_desc().await.unwrap().len(),
                        PRE_COUNT + TOTAL_RECOVERED_AND_BACKLOG
                    );
                }
            }

            // AC4 + external mirror: final authoritative DB highwater & mirror agree at `(1, 0)`.
            let expected_final_hw = CaptureQueueHighwater {
                segment_id: 1,
                byte_offset: 0,
            };
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                expected_final_hw
            );
            let mirror = read_highwater_mirror(&init.paths.root)
                .expect("read external highwater mirror")
                .expect("highwater mirror must exist");
            assert_eq!(mirror.get(lane), expected_final_hw);

            // AC5: post-commit safe reclamation updated dual `LTM2` (`MIN_LIVE_SEG = 1`,
            // removed `sealed[0]`) strictly BEFORE `unlink("events-00000000.log")` + `fsync_dir`.
            assert!(
                !seg0_path.exists(),
                "AC5 ({lane_label}): events-00000000.log must be unlinked after final DB commit"
            );
            assert!(seg1_path.exists());
            let final_meta = read_converged_lane_tail_meta(&lane_dir).unwrap();
            assert_eq!(final_meta.min_live_seg, 1);
            assert_eq!(final_meta.active_seg, 1);
            assert_eq!(final_meta.committed_len, 0);
            assert!(final_meta.sealed.is_empty());

            let pos_lock = obs
                .segment_events
                .iter()
                .position(|e| *e == SegmentOpEvent::AcquireAppendLock)
                .expect("AC5: reclaim must acquire append.lock");
            let pos_meta_primary = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::MetaRename {
                        lane: lane_label,
                        tmp_name: QUEUE_TAIL_META_TMP_FILE,
                        target_name: QUEUE_TAIL_META_FILE,
                    }
                })
                .expect("AC5: reclaim must atomically update primary tail.meta");
            let pos_fsync_primary = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                        lane: lane_label,
                        target_name: QUEUE_TAIL_META_FILE,
                    }
                })
                .expect("AC5: reclaim must fsync lane_dir after primary tail.meta");
            let pos_meta_bak = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::MetaRename {
                        lane: lane_label,
                        tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                        target_name: QUEUE_TAIL_META_BAK_FILE,
                    }
                })
                .expect("AC5: reclaim must atomically update backup tail.meta.bak");
            let pos_fsync_bak = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                        lane: lane_label,
                        target_name: QUEUE_TAIL_META_BAK_FILE,
                    }
                })
                .expect("AC5: reclaim must fsync lane_dir after backup tail.meta.bak");
            let pos_unlink = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::ReclaimSegmentUnlink {
                        lane: lane_label,
                        seg_id: 0,
                    }
                })
                .expect("AC5: reclaim must unlink events-00000000.log");
            let pos_fsync_reclaim = obs
                .segment_events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::FsyncLaneDirAfterReclaim {
                        lane: lane_label,
                        seg_id: 0,
                    }
                })
                .expect("AC5: reclaim must fsync lane_dir after unlink");
            let pos_unlock = obs
                .segment_events
                .iter()
                .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
                .expect("AC5: reclaim must release append.lock");
            assert!(
                pos_lock < pos_meta_primary
                    && pos_meta_primary < pos_fsync_primary
                    && pos_fsync_primary < pos_meta_bak
                    && pos_meta_bak < pos_fsync_bak
                    && pos_fsync_bak < pos_unlink
                    && pos_unlink < pos_fsync_reclaim
                    && pos_fsync_reclaim < pos_unlock,
                "AC5 ({lane_label}): reclaim must strictly update dual LTM2 before unlink + fsync_dir; got {:?}",
                obs.segment_events
            );
            assert!(probe_append_lock_is_free(&init.paths.root));
            assert!(!is_writer_active(repo.path()).unwrap());
        }
    }

    #[tokio::test]
    async fn consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append() {
        // `RB-09g` AC1 & AC2 (`ADR-RB-05` #4 ② (a)):
        // Verify on BOTH `normal/` and `priority/` lanes:
        // - AC1: Construct a pure synced uncommitted tail (`recovered_valid_end == actual_len`,
        //   zero damaged bytes, retaining `ACTIVE_SEG == S`) with `> 128` unconsumed
        //   frames in active segment `S`. Cover BOTH:
        //     (a) `RB-09e` `recover_lane_uncommitted_tail` + multi-batch drain across
        //         `50` committed backlog + `140` synced tail (`190 > 128` frames), and
        //     (b) direct `drain_locked_at_root` self-delegated recovery across `140`
        //         pure synced tail frames (`> 128` frames) after `AfterSegmentSyncData`.
        //   Verify every batch advances DB `QUEUE_HIGHWATER_*` strictly to
        //   `(S, batch_end_offset)` and stops at `(S, committed_len)` — NEVER jumping
        //   to `(S + 1, 0)`, ZERO quarantine artifacts, ZERO `events-00000001.log`.
        // - AC2: Immediately append `10` new frames into active segment `S` via public
        //   `append_postcard_frame`; verify the consumer drains seamlessly from
        //   `(S, old_committed_len)` with zero skipped frames, zero duplicates,
        //   zero highwater-regression errors, and 100% payload equality.
        use std::io::Write as _;

        const PRE_COUNT: usize = 3;
        const BACKLOG_COUNT: usize = 50;
        const TAIL_COUNT: usize = 140;
        const POST_APPEND_COUNT: usize = 10;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_080_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(
                    tag,
                    &format!("2026-11-24T07:{:02}:{:02}Z", (seq / 60) % 60, seq % 60),
                ),
            };

            // =================================================================
            // Sub-case (a): Pre-consumed + backlog + real `AfterSegmentSyncData`
            // pure synced tail (> 128 total), recovered via `recover_lane_uncommitted_tail`,
            // multi-batch drained on active `S = 0`, then subsequent append.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let mut hw_offset = 0u64;
                for i in 0..PRE_COUNT {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09g:ac1a:{lane_label}:pre:{i:03}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09g-ac1a-{lane_label}-pre-{i:03}"), i as i64),
                    )
                    .unwrap();
                    hw_offset = out.meta.committed_len;
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: hw_offset,
                    }
                );

                let mut expected_dedups = Vec::with_capacity(BACKLOG_COUNT + TAIL_COUNT);
                let mut expected_payloads = Vec::with_capacity(BACKLOG_COUNT + TAIL_COUNT);
                let mut frame_end_offsets = Vec::with_capacity(BACKLOG_COUNT + TAIL_COUNT);
                let mut cursor = hw_offset;
                for i in 0..BACKLOG_COUNT {
                    let dedup = format!("rb09g:ac1a:{lane_label}:backlog:{i:03}");
                    let payload = payload_for(
                        &format!("rb09g-ac1a-{lane_label}-backlog-{i:03}"),
                        (PRE_COUNT + i) as i64,
                    );
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .unwrap();
                    assert_eq!(out.frame_offset, cursor);
                    cursor = out.meta.committed_len;
                    expected_dedups.push(dedup);
                    expected_payloads.push(payload);
                    frame_end_offsets.push(cursor);
                }
                let committed_before_crash = cursor;

                // First tail frame via genuine `AfterSegmentSyncData` producer crash,
                // followed by `TAIL_COUNT - 1` more complete valid synced frames.
                let t0_dedup = format!("rb09g:ac1a:{lane_label}:tail:000");
                let t0_payload = payload_for(
                    &format!("rb09g-ac1a-{lane_label}-tail-000"),
                    (PRE_COUNT + BACKLOG_COUNT) as i64,
                );
                let t0_bytes = encode_postcard_frame(
                    flags,
                    &t0_dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &t0_payload,
                )
                .unwrap();
                let mut prod_crash_obs =
                    SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
                let _ = append_encoded_frame_in_storage_dir_with_observer(
                    repo.path(),
                    lane,
                    &t0_bytes,
                    QueueAppendOptions::default(),
                    &mut prod_crash_obs,
                )
                .expect_err("injected AfterSegmentSyncData must fail");
                cursor += t0_bytes.len() as u64;
                expected_dedups.push(t0_dedup);
                expected_payloads.push(t0_payload);
                frame_end_offsets.push(cursor);

                let mut extra_tail = Vec::new();
                for i in 1..TAIL_COUNT {
                    let dedup = format!("rb09g:ac1a:{lane_label}:tail:{i:03}");
                    let payload = payload_for(
                        &format!("rb09g-ac1a-{lane_label}-tail-{i:03}"),
                        (PRE_COUNT + BACKLOG_COUNT + i) as i64,
                    );
                    let raw = encode_postcard_frame(
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .unwrap();
                    cursor += raw.len() as u64;
                    extra_tail.extend_from_slice(&raw);
                    expected_dedups.push(dedup);
                    expected_payloads.push(payload);
                    frame_end_offsets.push(cursor);
                }
                let recovered_committed_len = cursor;
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .unwrap();
                    f.write_all(&extra_tail).unwrap();
                    f.sync_data().unwrap();
                }
                assert_eq!(
                    fs::metadata(&seg0_path).unwrap().len(),
                    recovered_committed_len
                );
                assert_eq!(
                    read_converged_lane_tail_meta(&lane_dir)
                        .unwrap()
                        .committed_len,
                    committed_before_crash
                );

                // Recover pure synced tail in place: keeps `active_seg == 0`, zero quarantine.
                let rec = CaptureQueueWriter::in_storage_dir(repo.path())
                    .recover_lane_uncommitted_tail(lane, &setup_spy)
                    .await
                    .unwrap()
                    .expect("pure synced tail must be recovered");
                assert_eq!(rec.recovered_frames, TAIL_COUNT);
                assert_eq!(rec.meta.active_seg, 0);
                assert_eq!(rec.meta.committed_len, recovered_committed_len);
                assert!(rec.meta.sealed.is_empty());
                assert!(rec.quarantined_artifacts.is_empty());
                assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
                assert!(!lane_dir.join(segment_log_file_name(1)).exists());

                // AC1: Multi-batch drain (> 128 frames) keeps `segment_id == 0` on EVERY batch
                // and stops strictly at `(0, recovered_committed_len)`.
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("AC1: multi-batch active drain after clean tail recovery must succeed");
                assert_eq!(rep.applied_frames, BACKLOG_COUNT + TAIL_COUNT);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 0);

                let calls = spy.recorded_calls();
                assert!(
                    calls.len() >= 2,
                    "AC1 ({lane_label}): 190 frames at max_batch_size=128 must span >= 2 batches"
                );
                let mut cum = 0usize;
                let mut seen_dedups = Vec::with_capacity(BACKLOG_COUNT + TAIL_COUNT);
                for (idx, call) in calls.iter().enumerate() {
                    assert_eq!(call.lane, lane);
                    assert_eq!(call.tx_delta, 1);
                    assert!(call.append_lock_was_free);
                    assert!((1..=DEFAULT_WRITER_MAX_BATCH_SIZE).contains(&call.item_count));
                    cum += call.item_count;
                    seen_dedups.extend(call.dedup_keys.iter().cloned());
                    let expected_end = frame_end_offsets[cum - 1];
                    assert_eq!(
                        call.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: 0,
                            byte_offset: expected_end,
                        },
                        "AC1 ({lane_label}): batch {idx} on active seg 0 must stay at (0, batch_end_offset), NEVER (1, 0)"
                    );
                }
                assert_eq!(cum, BACKLOG_COUNT + TAIL_COUNT);
                assert_eq!(seen_dedups, expected_dedups);
                let expected_hw_after_ac1 = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: recovered_committed_len,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_hw_after_ac1
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_hw_after_ac1
                );
                assert!(!lane_dir.join(segment_log_file_name(1)).exists());

                // AC2: Immediately append `POST_APPEND_COUNT` new frames to active seg 0
                // and verify seamless drain starting from `(0, recovered_committed_len)`.
                let mut post_dedups = Vec::with_capacity(POST_APPEND_COUNT);
                let mut post_payloads = Vec::with_capacity(POST_APPEND_COUNT);
                let mut post_cursor = recovered_committed_len;
                for i in 0..POST_APPEND_COUNT {
                    let dedup = format!("rb09g:ac2a:{lane_label}:post:{i:03}");
                    let payload = payload_for(
                        &format!("rb09g-ac2a-{lane_label}-post-{i:03}"),
                        (PRE_COUNT + BACKLOG_COUNT + TAIL_COUNT + i) as i64,
                    );
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .expect("AC2: subsequent append into active seg 0 must succeed");
                    assert_eq!(out.meta.active_seg, 0);
                    assert_eq!(out.frame_offset, post_cursor);
                    post_cursor = out.meta.committed_len;
                    post_dedups.push(dedup);
                    post_payloads.push(payload);
                }
                let final_committed_len = post_cursor;

                let post_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let post_rep = drain_capture_queue(repo.path(), &post_spy)
                    .await
                    .expect("AC2: drain after subsequent append to active seg 0 must succeed without regression");
                assert_eq!(post_rep.applied_frames, POST_APPEND_COUNT);
                assert_eq!(post_rep.deduplicated_frames, 0);
                let post_seen: Vec<String> = post_spy
                    .recorded_calls()
                    .into_iter()
                    .flat_map(|c| c.dedup_keys)
                    .collect();
                assert_eq!(post_seen, post_dedups);
                let expected_hw_final = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: final_committed_len,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_hw_final
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_hw_final
                );

                // Verify 100% payload equality across all recovered + post-append frames.
                for expected in expected_payloads.iter().chain(post_payloads.iter()) {
                    match expected {
                        CaptureBatchPayload::AppendAuditLog(rec) => {
                            let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        CaptureBatchPayload::UpsertSession(rec) => {
                            let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        other => panic!("unexpected payload: {other:?}"),
                    }
                }
            }

            // =================================================================
            // Sub-case (b): Direct `drain_capture_queue` self-delegating pure
            // synced tail recovery (`140 > 128` synced frames after drained prefix)
            // + immediate subsequent append.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let init_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let f0_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09g:ac1b:{lane_label}:pre:0"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09g-ac1b-{lane_label}-pre-0"), 0),
                )
                .unwrap();
                let mut cursor = f0_out.meta.committed_len;
                let _ = drain_capture_queue(repo.path(), &init_spy).await.unwrap();

                let mut raw_tail = Vec::new();
                let mut end_offsets = Vec::with_capacity(TAIL_COUNT);
                for i in 0..TAIL_COUNT {
                    let raw = encode_postcard_frame(
                        flags,
                        &format!("rb09g:ac1b:{lane_label}:tail:{i:03}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(
                            &format!("rb09g-ac1b-{lane_label}-tail-{i:03}"),
                            (1 + i) as i64,
                        ),
                    )
                    .unwrap();
                    cursor += raw.len() as u64;
                    raw_tail.extend_from_slice(&raw);
                    end_offsets.push(cursor);
                }
                let expected_end = cursor;
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(lane_dir.join(segment_log_file_name(0)))
                        .unwrap();
                    f.write_all(&raw_tail).unwrap();
                    f.sync_data().unwrap();
                }

                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("direct drain with >128 pure synced tail must succeed");
                assert_eq!(rep.recovery_delegations, 1);
                assert_eq!(rep.applied_frames, TAIL_COUNT);
                assert_eq!(fs::read_dir(&init.paths.quarantine).unwrap().count(), 0);
                let calls = spy.recorded_calls();
                assert!(calls.len() >= 2);
                let mut cum = 0usize;
                for call in &calls {
                    cum += call.item_count;
                    assert_eq!(
                        call.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: 0,
                            byte_offset: end_offsets[cum - 1],
                        }
                    );
                }
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: expected_end,
                    }
                );

                // Subsequent append seamlessly drains from `(0, expected_end)`.
                let post_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09g:ac2b:{lane_label}:post"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09g-ac2b-{lane_label}-post"), 999),
                )
                .unwrap();
                assert_eq!(post_out.meta.active_seg, 0);
                assert_eq!(post_out.frame_offset, expected_end);
                let post_rep = drain_capture_queue(repo.path(), &spy).await.unwrap();
                assert_eq!(post_rep.applied_frames, 1);
                assert_eq!(post_rep.deduplicated_frames, 0);
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: post_out.meta.committed_len,
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append() {
        // `RB-09g` AC3 & AC4 (`ADR-RB-05` #4 ② (a), (b)):
        // Verify on BOTH `normal/` and `priority/` lanes:
        // - AC3: Construct `LTM2.sealed.len() == 256` (`seg 0..=255`, with `seg 0..=254`
        //   carrying 2 frames each and `seg 255` carrying `130 > 128` frames to prove
        //   multi-batch sealed crossing as well) plus active segment `S = 256`
        //   containing `50` committed frames + `110` complete `sync_data` tail frames
        //   + damaged half-frame. Run Step C1 in-place recovery (`RB-09e`
        //   `recover_lane_uncommitted_tail` -> skips Step C2 because `sealed.len() == 256`,
        //   quarantines half-frame with `LBR-AGENT-050`, leaves `active_seg == 256`
        //   with `160 > 128` unconsumed frames). Consumer drains all 256 sealed
        //   segments in order (only advancing `(s+1, 0)` on the final batch exhausting
        //   `sealed_len(s)`), then drains active `S = 256` across `>= 2` batches
        //   keeping `QUEUE_HIGHWATER_*` strictly on `S = 256` and stopping at
        //   `(256, committed_len(256))` (NEVER `(257, 0)`).
        // - AC4: Immediately append `5` new frames to active `S = 256`; consumer
        //   drains all 5 frames seamlessly from `(256, old_committed_len)` with zero
        //   skipped frames, zero duplicates, zero highwater-regression errors, and
        //   100% payload equality across all 805 records.
        const SEALED_CAP: u32 = crate::storage::queue::MAX_SEALED_SEGMENTS as u32;
        const SMALL_SEALED_FRAMES: usize = 2;
        const LAST_SEALED_FRAMES: usize = 130;
        const ACTIVE_COMMITTED_FRAMES: usize = 50;
        const ACTIVE_TAIL_FRAMES: usize = 110;
        const ACTIVE_TOTAL_FRAMES: usize = ACTIVE_COMMITTED_FRAMES + ACTIVE_TAIL_FRAMES;
        const POST_APPEND_FRAMES: usize = 5;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_090_000 + seq)
                }
                CaptureQueueLane::Normal => make_audit_payload(
                    tag,
                    &format!("2026-11-24T08:{:02}:{:02}Z", (seq / 60) % 60, seq % 60),
                ),
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");

            // 1. Seed 256 sealed segments `0..=255` with authenticated `LTM2.sealed[]`
            //    entries (`seg 0..=254` have 2 frames each; `seg 255` has 130 > 128 frames).
            let mut sealed_entries = Vec::with_capacity(SEALED_CAP as usize);
            let mut expected_payloads: Vec<CaptureBatchPayload> = Vec::new();
            let mut seg255_end_offsets: Vec<u64> = Vec::with_capacity(LAST_SEALED_FRAMES);
            let mut seq_counter = 0i64;

            for seg_id in 0..SEALED_CAP {
                let frame_count = if seg_id + 1 == SEALED_CAP {
                    LAST_SEALED_FRAMES
                } else {
                    SMALL_SEALED_FRAMES
                };
                let mut seg_bytes = Vec::new();
                for f_idx in 0..frame_count {
                    let dedup = format!("rb09g:ac3:{lane_label}:s{seg_id:03}:f{f_idx:03}");
                    let payload = payload_for(
                        &format!("rb09g-ac3-{lane_label}-s{seg_id:03}-f{f_idx:03}"),
                        seq_counter,
                    );
                    seq_counter += 1;
                    let raw = encode_postcard_frame(
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .unwrap();
                    seg_bytes.extend_from_slice(&raw);
                    if seg_id + 1 == SEALED_CAP {
                        seg255_end_offsets.push(seg_bytes.len() as u64);
                    }
                    expected_payloads.push(payload);
                }
                write_bytes_0600(&lane_dir.join(segment_log_file_name(seg_id)), &seg_bytes);
                sealed_entries.push(SealedSegmentMeta {
                    seg_id,
                    sealed_len: seg_bytes.len() as u64,
                    sealed_crc32c: crc32c_checksum(&seg_bytes),
                });
            }
            assert_eq!(sealed_entries.len(), SEALED_CAP as usize);

            // 2. Seed active segment `S = 256` with `50` committed frames (`LTM2`) +
            //    `110` synced uncommitted tail frames + 1 damaged half-frame.
            let mut active_committed_bytes = Vec::new();
            let mut active_end_offsets: Vec<u64> = Vec::with_capacity(ACTIVE_TOTAL_FRAMES);
            for i in 0..ACTIVE_COMMITTED_FRAMES {
                let dedup = format!("rb09g:ac3:{lane_label}:s256:c{i:03}");
                let payload =
                    payload_for(&format!("rb09g-ac3-{lane_label}-s256-c{i:03}"), seq_counter);
                seq_counter += 1;
                let raw = encode_postcard_frame(
                    flags,
                    &dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload,
                )
                .unwrap();
                active_committed_bytes.extend_from_slice(&raw);
                active_end_offsets.push(active_committed_bytes.len() as u64);
                expected_payloads.push(payload);
            }
            let committed_len_before = active_committed_bytes.len() as u64;
            let prefix_crc_before = crc32c_checksum(&active_committed_bytes);

            let mut active_all_bytes = active_committed_bytes;
            for i in 0..ACTIVE_TAIL_FRAMES {
                let dedup = format!("rb09g:ac3:{lane_label}:s256:t{i:03}");
                let payload =
                    payload_for(&format!("rb09g-ac3-{lane_label}-s256-t{i:03}"), seq_counter);
                seq_counter += 1;
                let raw = encode_postcard_frame(
                    flags,
                    &dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload,
                )
                .unwrap();
                active_all_bytes.extend_from_slice(&raw);
                active_end_offsets.push(active_all_bytes.len() as u64);
                expected_payloads.push(payload);
            }
            let recovered_active_len = active_all_bytes.len() as u64;
            let half_full = encode_postcard_frame(
                flags,
                &format!("rb09g:ac3:{lane_label}:s256:half"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09g-ac3-{lane_label}-s256-half"), 99_999),
            )
            .unwrap();
            let damaged_half = half_full[..half_full.len() / 2].to_vec();
            active_all_bytes.extend_from_slice(&damaged_half);

            let seg256_path = lane_dir.join(segment_log_file_name(SEALED_CAP));
            let seg257_path = lane_dir.join(segment_log_file_name(SEALED_CAP + 1));
            write_bytes_0600(&seg256_path, &active_all_bytes);
            {
                let f = fs::OpenOptions::new()
                    .write(true)
                    .open(&seg256_path)
                    .unwrap();
                f.sync_data().unwrap();
            }

            let initial_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: SEALED_CAP,
                committed_len: committed_len_before,
                prefix_crc32c: prefix_crc_before,
                sealed: sealed_entries.clone(),
            };
            let raw_meta = initial_meta.encode().expect("encode full 256-entry LTM2");
            write_bytes_0600(&tail_meta_path(&lane_dir), &raw_meta);
            write_bytes_0600(&tail_meta_bak_path(&lane_dir), &raw_meta);

            // 3. Step C1 in-place recovery under `sealed.len() == 256`: MUST keep
            //    `active_seg == 256`, `sealed.len() == 256`, quarantine ONLY damaged half,
            //    and NOT create `seg 257`!
            let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
            let rec = CaptureQueueWriter::in_storage_dir(repo.path())
                .recover_lane_uncommitted_tail(lane, &setup_spy)
                .await
                .expect("Step C1 recovery on full 256 table must succeed")
                .expect("must recover active seg 256 in place");
            assert_eq!(rec.recovered_frames, ACTIVE_TAIL_FRAMES);
            assert_eq!(rec.meta.min_live_seg, 0);
            assert_eq!(rec.meta.active_seg, SEALED_CAP);
            assert_eq!(rec.meta.committed_len, recovered_active_len);
            assert_eq!(rec.meta.sealed.len(), SEALED_CAP as usize);
            assert_eq!(rec.quarantined_artifacts.len(), 1);
            assert_eq!(
                rec.quarantined_artifacts[0].diagnostic_code,
                QUARANTINE_DIAGNOSTIC_CODE
            );
            assert_eq!(
                fs::read(&rec.quarantined_artifacts[0].bin_path).unwrap(),
                damaged_half
            );
            assert!(!seg257_path.exists());
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater::default()
            );

            // 4. AC3: Drain all 256 sealed segments + active `seg 256` (> 128 frames).
            let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
            let rep = drain_capture_queue(repo.path(), &spy).await.expect(
                "AC3: drain across 256 sealed segments + recovered active S=256 must succeed",
            );
            let total_before_post = (SEALED_CAP as usize - 1) * SMALL_SEALED_FRAMES
                + LAST_SEALED_FRAMES
                + ACTIVE_TOTAL_FRAMES;
            assert_eq!(rep.applied_frames, total_before_post);
            assert_eq!(rep.deduplicated_frames, 0);
            assert_eq!(rep.reclaimed_segments, SEALED_CAP as usize);

            // Verify every single batch transition across `seg 0..=255` and active `seg 256`:
            let calls = spy.recorded_calls();
            let mut call_idx = 0usize;
            for seg_id in 0..(SEALED_CAP - 1) {
                let mut seg_frames = 0usize;
                while seg_frames < SMALL_SEALED_FRAMES {
                    let c = &calls[call_idx];
                    call_idx += 1;
                    assert_eq!(c.lane, lane);
                    assert_eq!(c.tx_delta, 1);
                    assert!(c.append_lock_was_free);
                    seg_frames += c.item_count;
                    if seg_frames < SMALL_SEALED_FRAMES {
                        assert_eq!(c.db_highwater_after.segment_id, seg_id);
                        assert!(
                            c.db_highwater_after.byte_offset
                                < sealed_entries[seg_id as usize].sealed_len
                        );
                    } else {
                        assert_eq!(
                            c.db_highwater_after,
                            CaptureQueueHighwater {
                                segment_id: seg_id + 1,
                                byte_offset: 0,
                            },
                            "AC3 ({lane_label}): final batch of sealed seg {seg_id} must cross to ({}, 0)",
                            seg_id + 1
                        );
                    }
                }
            }

            // Verify multi-batch sealed `seg 255` (130 frames => >= 2 batches):
            let mut seg255_batches = 0usize;
            let mut seg255_frames = 0usize;
            while seg255_frames < LAST_SEALED_FRAMES {
                let c = &calls[call_idx];
                call_idx += 1;
                seg255_batches += 1;
                seg255_frames += c.item_count;
                if seg255_frames < LAST_SEALED_FRAMES {
                    assert_eq!(
                        c.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: SEALED_CAP - 1,
                            byte_offset: seg255_end_offsets[seg255_frames - 1],
                        },
                        "AC3 ({lane_label}): intermediate batch of sealed seg 255 must NOT jump to (256, 0)"
                    );
                } else {
                    assert_eq!(
                        c.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: SEALED_CAP,
                            byte_offset: 0,
                        },
                        "AC3 ({lane_label}): final batch of sealed seg 255 must advance atomically to (256, 0)"
                    );
                }
            }
            assert!(seg255_batches >= 2);

            // Verify multi-batch active `seg 256` (160 frames => >= 2 batches):
            // EVERY batch must remain on `segment_id == 256` and stop at `(256, recovered_active_len)`!
            let active_calls = &calls[call_idx..];
            assert!(
                active_calls.len() >= 2,
                "AC3 ({lane_label}): 160 active frames in seg 256 must require >= 2 batches"
            );
            let mut active_cum = 0usize;
            for (i, c) in active_calls.iter().enumerate() {
                assert_eq!(c.lane, lane);
                assert_eq!(c.tx_delta, 1);
                assert!(c.append_lock_was_free);
                active_cum += c.item_count;
                assert_eq!(
                    c.db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: SEALED_CAP,
                        byte_offset: active_end_offsets[active_cum - 1],
                    },
                    "AC3 ({lane_label}): active seg 256 batch {i} must stay on seg 256, NEVER jump to (257, 0)"
                );
            }
            assert_eq!(active_cum, ACTIVE_TOTAL_FRAMES);
            let expected_hw_after_ac3 = CaptureQueueHighwater {
                segment_id: SEALED_CAP,
                byte_offset: recovered_active_len,
            };
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                expected_hw_after_ac3
            );
            assert_eq!(
                read_highwater_mirror(&init.paths.root)
                    .unwrap()
                    .unwrap()
                    .get(lane),
                expected_hw_after_ac3
            );
            assert!(!seg257_path.exists());
            let meta_after_ac3 = read_converged_lane_tail_meta(&lane_dir).unwrap();
            assert_eq!(meta_after_ac3.min_live_seg, SEALED_CAP);
            assert_eq!(meta_after_ac3.active_seg, SEALED_CAP);
            assert_eq!(meta_after_ac3.committed_len, recovered_active_len);
            assert!(meta_after_ac3.sealed.is_empty());

            // 5. AC4: Immediately append `POST_APPEND_FRAMES` new frames to active `S = 256`
            //    and confirm seamless drain with 0 skipped frames, 0 dups, 0 regression error.
            let mut post_cursor = recovered_active_len;
            for i in 0..POST_APPEND_FRAMES {
                let dedup = format!("rb09g:ac4:{lane_label}:s256:post:{i:03}");
                let payload = payload_for(
                    &format!("rb09g-ac4-{lane_label}-s256-post-{i:03}"),
                    seq_counter,
                );
                seq_counter += 1;
                let out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &dedup,
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload,
                )
                .expect("AC4: subsequent append into active seg 256 must succeed");
                assert_eq!(out.meta.active_seg, SEALED_CAP);
                assert_eq!(out.frame_offset, post_cursor);
                post_cursor = out.meta.committed_len;
                expected_payloads.push(payload);
            }
            let final_len = post_cursor;

            let post_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
            let post_rep = drain_capture_queue(repo.path(), &post_spy)
                .await
                .expect("AC4: drain of post-recovery appends on active seg 256 must succeed");
            assert_eq!(post_rep.applied_frames, POST_APPEND_FRAMES);
            assert_eq!(post_rep.deduplicated_frames, 0);
            assert_eq!(
                store.get_queue_highwater(lane).await.unwrap(),
                CaptureQueueHighwater {
                    segment_id: SEALED_CAP,
                    byte_offset: final_len,
                }
            );
            assert!(!seg257_path.exists());

            // Verify 100% payload equality for all 805 persisted records.
            assert_eq!(
                expected_payloads.len(),
                total_before_post + POST_APPEND_FRAMES
            );
            for expected in &expected_payloads {
                match expected {
                    CaptureBatchPayload::AppendAuditLog(rec) => {
                        let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                        assert_eq!(&got, rec);
                    }
                    CaptureBatchPayload::UpsertSession(rec) => {
                        let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                        assert_eq!(&got, rec);
                    }
                    other => panic!("unexpected payload: {other:?}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append() {
        // `RB-09g` AC5 & AC6 (`ADR-RB-05` #4 ② (a)):
        // Verify on BOTH `normal/` and `priority/` lanes AND BOTH `S = 0` (virgin
        // first-frame crash) and `S = 1` (post-rotation first-frame crash):
        // - AC5: Construct a first-frame partial crash truncated to exact 0 bytes by
        //   Step C1 (`recovered_valid_end == 0`, `ACTIVE_SEG == S`, quarantined with
        //   `LBR-AGENT-050`, no `events-<S+1>.log` created). Consumer run leaves DB
        //   `QUEUE_HIGHWATER_*` strictly at `(S, 0)` (NEVER jumping to `(S + 1, 0)`).
        // - AC6: Immediately append valid new frames into active segment `S`; consumer
        //   drains 100% of them seamlessly from `(S, 0)` with zero skipped frames,
        //   zero duplicates, zero highwater-regression errors, and full payload equality.
        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_100_000 + seq)
                }
                CaptureQueueLane::Normal => {
                    make_audit_payload(tag, &format!("2026-11-24T09:00:{:02}Z", seq % 60))
                }
            };

            for start_seg in [0u32, 1u32] {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                if start_seg == 1 {
                    // Advance naturally to `(1, 0)` with `min_live_seg = 1, active_seg = 1,
                    // committed_len = 0` by sealing & draining `seg 0` first.
                    let s0_bytes = encode_postcard_frame(
                        flags,
                        &format!("rb09g:ac5:{lane_label}:s0:pre"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09g-ac5-{lane_label}-s0-pre"), 0),
                    )
                    .unwrap();
                    write_bytes_0600(&lane_dir.join(segment_log_file_name(0)), &s0_bytes);
                    write_bytes_0600(&lane_dir.join(segment_log_file_name(1)), &[]);
                    let meta = LaneTailMeta {
                        min_live_seg: 0,
                        active_seg: 1,
                        committed_len: 0,
                        prefix_crc32c: 0,
                        sealed: vec![SealedSegmentMeta {
                            seg_id: 0,
                            sealed_len: s0_bytes.len() as u64,
                            sealed_crc32c: crc32c_checksum(&s0_bytes),
                        }],
                    };
                    let raw = meta.encode().unwrap();
                    write_bytes_0600(&tail_meta_path(&lane_dir), &raw);
                    write_bytes_0600(&tail_meta_bak_path(&lane_dir), &raw);

                    let pre_rep = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                    assert_eq!(pre_rep.applied_frames, 1);
                    assert_eq!(pre_rep.reclaimed_segments, 1);
                    assert_eq!(
                        store.get_queue_highwater(lane).await.unwrap(),
                        CaptureQueueHighwater {
                            segment_id: 1,
                            byte_offset: 0,
                        }
                    );
                } else {
                    // Virgin `S = 0` pre-bootstrapped `(0, 0, 0, 0, [])` with empty `seg 0`.
                    let init_meta = LaneTailMeta::initial_empty();
                    let raw = init_meta.encode().unwrap();
                    write_bytes_0600(&tail_meta_path(&lane_dir), &raw);
                    write_bytes_0600(&tail_meta_bak_path(&lane_dir), &raw);
                }

                // Inject first-frame partial crash bytes into `events-<start_seg>.log`
                // (`actual_len > 0`, `committed_len == 0`, `recovered_valid_end == 0`).
                let half_full = encode_postcard_frame(
                    flags,
                    &format!("rb09g:ac5:{lane_label}:s{start_seg}:first-half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09g-ac5-{lane_label}-s{start_seg}-half"), 1),
                )
                .unwrap();
                let damaged_first = half_full[..half_full.len() / 2].to_vec();
                let seg_path = lane_dir.join(segment_log_file_name(start_seg));
                let next_seg_path = lane_dir.join(segment_log_file_name(start_seg + 1));
                write_bytes_0600(&seg_path, &damaged_first);

                // AC5: Consumer delegates Step C1 0-byte recovery; DB `QUEUE_HIGHWATER_*`
                // strictly remains `(start_seg, 0)` (NEVER jumps to `(start_seg + 1, 0)`!).
                let tx_before = store.write_transaction_count();
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("AC5: consumer drain over 0-byte first-frame crash must succeed");
                assert_eq!(rep.recovery_delegations, 1);
                assert_eq!(rep.applied_frames, 0);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(
                    store.write_transaction_count(),
                    tx_before,
                    "AC5 ({lane_label}, S={start_seg}): 0-byte recovery must not write any DB tx"
                );
                assert!(spy.recorded_calls().is_empty());

                let expected_zero_hw = CaptureQueueHighwater {
                    segment_id: start_seg,
                    byte_offset: 0,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_zero_hw,
                    "AC5 ({lane_label}, S={start_seg}): DB highwater must stay strictly at (S, 0), NEVER (S+1, 0)"
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_zero_hw
                );
                assert_eq!(fs::metadata(&seg_path).unwrap().len(), 0);
                assert!(!next_seg_path.exists());
                let meta_zero = read_converged_lane_tail_meta(&lane_dir).unwrap();
                assert_eq!(meta_zero.min_live_seg, start_seg);
                assert_eq!(meta_zero.active_seg, start_seg);
                assert_eq!(meta_zero.committed_len, 0);
                assert_eq!(meta_zero.prefix_crc32c, 0);
                assert!(meta_zero.sealed.is_empty());
                // Quarantine holds the exact damaged first-frame artifact.
                let q_entries: Vec<_> = fs::read_dir(&init.paths.quarantine)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                assert_eq!(q_entries.len(), 1);
                assert_eq!(fs::read(&q_entries[0]).unwrap(), damaged_first);

                // AC6: Immediately append 5 valid frames into active `start_seg` and
                // confirm 100% seamless ingestion from `(start_seg, 0)`.
                let mut post_payloads = Vec::with_capacity(5);
                let mut post_dedups = Vec::with_capacity(5);
                let mut cursor = 0u64;
                for i in 0..5usize {
                    let dedup = format!("rb09g:ac6:{lane_label}:s{start_seg}:post:{i:02}");
                    let payload = payload_for(
                        &format!("rb09g-ac6-{lane_label}-s{start_seg}-post-{i:02}"),
                        10 + i as i64,
                    );
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .expect("AC6: append after 0-byte recovery must succeed on active seg S");
                    assert_eq!(out.meta.active_seg, start_seg);
                    assert_eq!(out.frame_offset, cursor);
                    cursor = out.meta.committed_len;
                    post_dedups.push(dedup);
                    post_payloads.push(payload);
                }
                let final_len = cursor;

                let post_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let post_rep = drain_capture_queue(repo.path(), &post_spy)
                    .await
                    .expect("AC6: consumer drain after 0-byte recovery + append must succeed");
                assert_eq!(post_rep.applied_frames, 5);
                assert_eq!(post_rep.deduplicated_frames, 0);
                let seen: Vec<String> = post_spy
                    .recorded_calls()
                    .into_iter()
                    .flat_map(|c| c.dedup_keys)
                    .collect();
                assert_eq!(seen, post_dedups);
                let expected_final_hw = CaptureQueueHighwater {
                    segment_id: start_seg,
                    byte_offset: final_len,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_final_hw
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_final_hw
                );
                assert!(!next_seg_path.exists());
                for expected in &post_payloads {
                    match expected {
                        CaptureBatchPayload::AppendAuditLog(rec) => {
                            let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        CaptureBatchPayload::UpsertSession(rec) => {
                            let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        other => panic!("unexpected payload: {other:?}"),
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance() {
        // `RB-09h` AC1 & AC2 (`ADR-RB-05` #4 ② (c)):
        // Verify on BOTH `normal/` and `priority/` lanes:
        // - AC1 (16 MiB regular rotation interleaving): Consumer drains active
        //   segment `S = 0` to `(0, committed_len)` (~15 MiB via 5 real ~3 MiB
        //   frames). Producer then triggers a genuine 16 MiB regular rotation
        //   (`CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES`) appending a 6th ~2 MiB frame
        //   into `events-00000001.log` and sealing `seg 0` in `LTM2.sealed[]`.
        //   Consumer authenticates `LTM2.sealed[0]`, executes a single-tx atomic
        //   zero-frame `CaptureStore::append_capture_batch(&[], (1, 0))` (0 items,
        //   0 redundant business rows, `tx_delta == 1`, DB hw == `(1, 0)`), then
        //   drains `seg 1` cleanly.
        // - AC2 (Case C Step C2 rotation interleaving): Consumer drains active
        //   segment `S = 0` to `(0, committed_len)`. A damaged half-frame crash
        //   tail is appended, and producer recovery (`append_postcard_frame` in
        //   sub-case 2a and `recover_lane` in sub-case 2b) runs Case C Step C2
        //   sealing `seg 0` at `sealed_len == old_committed_len` and rotating to
        //   `S + 1 = 1`. Consumer authenticates `LTM2.sealed[0]` and executes
        //   single-tx `CaptureStore::append_capture_batch(&[], (1, 0))` with ZERO
        //   redundant business rows.
        use std::io::Write as _;

        let row_count_for = |store: RedbCaptureStore, lane: CaptureQueueLane| async move {
            match lane {
                CaptureQueueLane::Normal => store
                    .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                    .await
                    .unwrap()
                    .len(),
                CaptureQueueLane::Priority => {
                    store.list_sessions_by_started_desc().await.unwrap().len()
                }
            }
        };

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let sized_payload_for = |tag: &str, seq: i64, pad_bytes: usize| match lane {
                CaptureQueueLane::Priority => {
                    let mut rec = match make_session_payload(
                        tag,
                        CaptureSessionState::Stopped,
                        1_700_110_000 + seq,
                    ) {
                        CaptureBatchPayload::UpsertSession(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.metadata_json = format!(r#"{{"pad":"{}"}}"#, "p".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::UpsertSession(rec)
                }
                CaptureQueueLane::Normal => {
                    let mut rec = match make_audit_payload(
                        tag,
                        &format!("2026-11-24T10:00:{:02}Z", seq % 60),
                    ) {
                        CaptureBatchPayload::AppendAuditLog(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.justification = Some("n".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::AppendAuditLog(rec)
                }
            };

            // =================================================================
            // AC1: Drained active `S = 0` -> genuine 16 MiB regular rotation ->
            // atomic zero-frame `append_capture_batch(&[], (1, 0))` + 0 extra rows.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                // Append 5 × 3 MiB frames (~15 MiB < 16 MiB) staying inside `seg 0`.
                let mut seg0_committed_len = 0u64;
                for i in 0..5usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09h:ac1:{lane_label}:big:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09h-ac1-{lane_label}-big-{i}"),
                            i as i64,
                            3 * 1024 * 1024,
                        ),
                    )
                    .expect("enqueue 3 MiB frame inside seg 0");
                    assert_eq!(out.meta.active_seg, 0);
                    seg0_committed_len = out.meta.committed_len;
                }
                assert!(
                    seg0_committed_len > 15 * 1024 * 1024
                        && seg0_committed_len
                            < crate::storage::queue::CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES,
                    "seg 0 should sit between 15 MiB and 16 MiB before rotation"
                );

                // Drain active `seg 0` completely: DB highwater stops at `(0, seg0_committed_len)`.
                let pre_rep = drain_capture_queue(repo.path(), &setup_spy)
                    .await
                    .expect("drain 5 x 3 MiB active seg 0 frames");
                assert_eq!(pre_rep.applied_frames, 5);
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: seg0_committed_len,
                    }
                );
                assert_eq!(row_count_for(store.clone(), lane).await, 5);

                // Producer appends a 6th ~2 MiB frame -> exceeds 16 MiB -> REAL 16 MiB rotation!
                let rot_payload =
                    sized_payload_for(&format!("rb09h-ac1-{lane_label}-rot-5"), 5, 2 * 1024 * 1024);
                let rot_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09h:ac1:{lane_label}:rot:5"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &rot_payload,
                )
                .expect("6th frame crossing 16 MiB must trigger real regular rotation");
                assert_eq!(rot_out.meta.active_seg, 1);
                assert_eq!(rot_out.meta.min_live_seg, 0);
                assert_eq!(rot_out.meta.sealed.len(), 1);
                assert_eq!(rot_out.meta.sealed[0].seg_id, 0);
                assert_eq!(rot_out.meta.sealed[0].sealed_len, seg0_committed_len);
                assert!(lane_dir.join(segment_log_file_name(0)).exists());
                assert!(lane_dir.join(segment_log_file_name(1)).exists());

                // Run consumer: first tx MUST be `append_capture_batch(&[], (1, 0))`
                // with 0 items and 0 redundant business rows added!
                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &spy).await.expect(
                    "AC1: consumer empty-advance + seg 1 drain after 16 MiB rotation must succeed",
                );
                assert_eq!(rep.applied_frames, 1);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);

                let calls = spy.recorded_calls();
                assert_eq!(
                    calls.len(),
                    2,
                    "AC1 ({lane_label}): expected [empty-advance (1,0), seg 1 batch]"
                );
                assert_eq!(calls[0].lane, lane);
                assert_eq!(
                    calls[0].item_count, 0,
                    "AC1 ({lane_label}): first batch must be zero-frame empty advance"
                );
                assert_eq!(calls[0].applied_count, 0);
                assert_eq!(calls[0].deduplicated_count, 0);
                assert!(calls[0].dedup_keys.is_empty());
                assert_eq!(calls[0].tx_delta, 1);
                assert!(calls[0].append_lock_was_free);
                assert_eq!(
                    calls[0].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    },
                    "AC1 ({lane_label}): empty-advance tx must atomically advance DB highwater to (1, 0)"
                );

                assert_eq!(calls[1].lane, lane);
                assert_eq!(calls[1].item_count, 1);
                assert_eq!(calls[1].applied_count, 1);
                assert_eq!(calls[1].deduplicated_count, 0);
                assert_eq!(calls[1].tx_delta, 1);
                assert_eq!(
                    calls[1].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: rot_out.meta.committed_len,
                    }
                );
                assert_eq!(
                    row_count_for(store.clone(), lane).await,
                    6,
                    "AC1 ({lane_label}): exact 5 + 1 = 6 business rows (zero redundant rows)"
                );
                assert!(!lane_dir.join(segment_log_file_name(0)).exists());
            }

            // =================================================================
            // AC2a: Drained active `S = 0` -> producer `append_postcard_frame`
            // triggers Case C Step C2 sealing `S = 0` and writing `S + 1 = 1`.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let mut l3 = 0u64;
                for i in 0..3usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09h:ac2a:{lane_label}:pre:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09h-ac2a-{lane_label}-pre-{i}"),
                            i as i64,
                            0,
                        ),
                    )
                    .unwrap();
                    l3 = out.meta.committed_len;
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: l3,
                    }
                );
                assert_eq!(row_count_for(store.clone(), lane).await, 3);

                // Inject damaged half-frame tail after drained `l3`.
                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09h:ac2a:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(&format!("rb09h-ac2a-{lane_label}-half"), 99, 0),
                )
                .unwrap();
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(lane_dir.join(segment_log_file_name(0)))
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }

                // Producer appends new frame -> triggers Case C Step C2 sealing `seg 0`
                // at `l3` and creating `seg 1` with the new frame!
                let next_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09h:ac2a:{lane_label}:s1:0"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(&format!("rb09h-ac2a-{lane_label}-s1-0"), 10, 0),
                )
                .expect("producer append must run Case C Step C2 and rotate to seg 1");
                assert_eq!(next_out.meta.active_seg, 1);
                assert_eq!(next_out.meta.sealed.len(), 1);
                assert_eq!(next_out.meta.sealed[0].seg_id, 0);
                assert_eq!(next_out.meta.sealed[0].sealed_len, l3);

                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &spy).await.expect(
                    "AC2a: consumer empty-advance + seg 1 drain after Step C2 must succeed",
                );
                assert_eq!(rep.applied_frames, 1);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);

                let calls = spy.recorded_calls();
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].item_count, 0);
                assert!(calls[0].dedup_keys.is_empty());
                assert_eq!(calls[0].tx_delta, 1);
                assert!(calls[0].append_lock_was_free);
                assert_eq!(
                    calls[0].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(calls[1].item_count, 1);
                assert_eq!(
                    calls[1].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: next_out.meta.committed_len,
                    }
                );
                assert_eq!(row_count_for(store.clone(), lane).await, 4);
            }

            // =================================================================
            // AC2b: Drained active `S = 0` -> Case C Step C2 leaves empty `S + 1 = 1`
            // (0 frames in `seg 1`) -> consumer runs EXACTLY ONE zero-frame tx
            // advancing DB highwater to `(1, 0)` with 0 business rows added!
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let mut l2 = 0u64;
                for i in 0..2usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09h:ac2b:{lane_label}:pre:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09h-ac2b-{lane_label}-pre-{i}"),
                            i as i64,
                            0,
                        ),
                    )
                    .unwrap();
                    l2 = out.meta.committed_len;
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                assert_eq!(row_count_for(store.clone(), lane).await, 2);

                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09h:ac2b:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(&format!("rb09h-ac2b-{lane_label}-half"), 99, 0),
                )
                .unwrap();
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(lane_dir.join(segment_log_file_name(0)))
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }

                // Trigger Case C Step C2 via `recover_lane` -> seals `seg 0` at `l2` and creates empty `seg 1`.
                let rec = crate::storage::queue::recover_lane(repo.path(), lane).unwrap();
                assert_eq!(rec.meta.active_seg, 1);
                assert_eq!(rec.meta.committed_len, 0);
                assert_eq!(rec.meta.sealed.len(), 1);
                assert_eq!(rec.meta.sealed[0].sealed_len, l2);

                let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let tx_before = store.write_transaction_count();
                let rep = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect("AC2b: consumer single zero-frame advance to empty seg 1 must succeed");
                let tx_after = store.write_transaction_count();

                assert_eq!(tx_after - tx_before, 1);
                assert_eq!(rep.applied_frames, 0);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);
                let calls = spy.recorded_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].item_count, 0);
                assert!(calls[0].dedup_keys.is_empty());
                assert_eq!(calls[0].tx_delta, 1);
                assert!(calls[0].append_lock_was_free);
                assert_eq!(
                    calls[0].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(
                    row_count_for(store.clone(), lane).await,
                    2,
                    "AC2b ({lane_label}): zero-frame advance must insert ZERO redundant business rows"
                );
            }
        }
    }

    #[tokio::test]
    async fn consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected() {
        // `RB-09h` AC3–AC6 (`ADR-RB-05` #4 ② (c)):
        // Verify on BOTH `normal/` and `priority/` lanes that in the drained-then-rotated
        // state (`S < active_seg && hw_offset == sealed_len(S)`), BEFORE executing any
        // zero-frame highwater advance:
        // - AC3: Bitflipping ANY byte (first / middle / last) of `events-<S>.log`
        //   fails closed with `CorruptCommittedFrame` (`LBR-IO-001`), 0 DB tx, 0 HW advance,
        //   and 0 disk mutation.
        // - AC4: Truncating `events-<S>.log` — including exact complete-frame `CleanEof`
        //   boundary truncation (explicitly verified with `FrameStreamDecoder`), mid-frame
        //   truncation, and 0-byte truncation — fails closed with `CorruptCommittedFrame`
        //   (`LBR-IO-001`), 0 DB tx, 0 HW advance, and 0 disk mutation.
        // - AC5: Deleting `events-<S>.log` fails closed with `CorruptCommittedFrame`
        //   (`LBR-IO-001`), 0 DB tx, 0 HW advance, and 0 disk mutation.
        // - AC6: Out-of-bounds DB highwater `hw_offset > LTM2.sealed[S].sealed_len`
        //   (`+1` and `+64`) immediately fails closed with `CorruptCommittedFrame`
        //   (`LBR-IO-001`), 0 DB tx, 0 HW advance, and 0 disk mutation (plus direct
        //   `compute_next_highwater` unit guard).
        use std::io::Write as _;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_120_000 + seq)
                }
                CaptureQueueLane::Normal => {
                    make_audit_payload(tag, &format!("2026-11-24T11:00:{:02}Z", seq % 60))
                }
            };

            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            bootstrap_writer_lock(&init.paths.root);
            let lane_dir = match lane {
                CaptureQueueLane::Normal => init.paths.normal.clone(),
                CaptureQueueLane::Priority => init.paths.priority.clone(),
            };
            let store = RedbCaptureStore::create(
                repo.path()
                    .join(PRIVATE_SUBDIR)
                    .join("capture.gen-0001.redb"),
            )
            .await
            .expect("create RedbCaptureStore");
            let spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

            // Seed 3 frames into `seg 0`, recording exact frame boundaries `[l1, l2, l3]`.
            let mut offsets = Vec::with_capacity(3);
            for i in 0..3usize {
                let out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09h:guard:{lane_label}:s0:{i}"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09h-guard-{lane_label}-s0-{i}"), i as i64),
                )
                .unwrap();
                offsets.push(out.meta.committed_len);
            }
            let l2 = offsets[1];
            let sealed_len = offsets[2];

            // Drain active `seg 0` -> DB `hw` sits at `(0, sealed_len)`.
            let pre_rep = drain_capture_queue(repo.path(), &spy).await.unwrap();
            assert_eq!(pre_rep.applied_frames, 3);
            let hw_drained = CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: sealed_len,
            };
            assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_drained);

            // Trigger Case C Step C2 sealing `seg 0` at `sealed_len` and creating `seg 1` with 1 frame.
            let half = encode_postcard_frame(
                flags,
                &format!("rb09h:guard:{lane_label}:half"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09h-guard-{lane_label}-half"), 99),
            )
            .unwrap();
            let seg0_path = lane_dir.join(segment_log_file_name(0));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&half[..half.len() / 2]).unwrap();
                f.sync_data().unwrap();
            }
            let rot_out = append_postcard_frame(
                repo.path(),
                lane,
                flags,
                &format!("rb09h:guard:{lane_label}:s1:0"),
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload_for(&format!("rb09h-guard-{lane_label}-s1-0"), 10),
            )
            .unwrap();
            assert_eq!(rot_out.meta.active_seg, 1);
            assert_eq!(rot_out.meta.sealed.len(), 1);
            assert_eq!(rot_out.meta.sealed[0].sealed_len, sealed_len);

            let orig_seg0 = fs::read(&seg0_path).unwrap();
            assert_eq!(orig_seg0.len() as u64, sealed_len);
            let mirror_path = highwater_mirror_path(&init.paths.root);
            let orig_mirror = fs::read(&mirror_path).unwrap();

            let assert_rejected =
                |err: StorageBackendError,
                 label: &str,
                 tx_before: u64,
                 calls_before: usize,
                 expected_hw: CaptureQueueHighwater,
                 tree_before: &std::collections::BTreeMap<String, Vec<u8>>| {
                    assert_corrupt_committed_frame(err, label);
                    assert_eq!(
                        store.write_transaction_count(),
                        tx_before,
                        "{label}: zero DB write transactions allowed on fail-closed reject"
                    );
                    assert_eq!(
                        spy.recorded_calls().len(),
                        calls_before,
                        "{label}: zero append_capture_batch calls allowed on fail-closed reject"
                    );
                    assert_eq!(
                        fs::read(&mirror_path).unwrap(),
                        orig_mirror,
                        "{label}: external highwater mirror must remain unchanged"
                    );
                    assert_eq!(
                        snapshot_queue_tree_full(&init.paths.root),
                        *tree_before,
                        "{label}: entire queue directory tree must remain byte-identical"
                    );
                    let _ = expected_hw;
                };

            // =================================================================
            // AC3: Bitflip first, middle, and last byte of `events-00000000.log`.
            // =================================================================
            for (pos_label, pos) in [
                ("first-byte", 0usize),
                ("middle-byte", orig_seg0.len() / 2),
                ("last-byte", orig_seg0.len() - 1),
            ] {
                let mut flipped = orig_seg0.clone();
                flipped[pos] ^= 0x01;
                write_bytes_0600(&seg0_path, &flipped);
                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let err = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect_err("AC3: bitflip in drained sealed seg 0 must fail closed");
                assert_rejected(
                    err,
                    &format!("AC3 ({lane_label}, {pos_label})"),
                    tx_before,
                    calls_before,
                    hw_drained,
                    &tree_before,
                );
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_drained);
            }

            // =================================================================
            // AC4: Truncation — (a) exact complete-frame `CleanEof` boundary `l2`,
            // (b) mid-frame truncation, (c) 0-byte truncation.
            // =================================================================
            {
                let truncated_clean = orig_seg0[..l2 as usize].to_vec();
                // Explicitly confirm `FrameStreamDecoder` yields 2 valid frames + `CleanEof(l2)`!
                let mut cursor = std::io::Cursor::new(&truncated_clean);
                let mut dec = FrameStreamDecoder::with_known_len(&mut cursor, 0, l2);
                let mut decoded_count = 0usize;
                loop {
                    match dec
                        .next_frame()
                        .expect("complete-frame truncation decodes cleanly")
                    {
                        FrameStreamItem::Frame { .. } => decoded_count += 1,
                        FrameStreamItem::CleanEof { offset } => {
                            assert_eq!(offset, l2);
                            break;
                        }
                    }
                }
                assert_eq!(decoded_count, 2);

                for (trunc_label, trunc_len) in [
                    ("complete-frame-CleanEof", l2 as usize),
                    ("mid-frame", (l2 as usize) + 10),
                    ("zero-byte", 0usize),
                ] {
                    write_bytes_0600(&seg0_path, &orig_seg0[..trunc_len]);
                    let tree_before = snapshot_queue_tree_full(&init.paths.root);
                    let tx_before = store.write_transaction_count();
                    let calls_before = spy.recorded_calls().len();
                    let err = drain_capture_queue(repo.path(), &spy)
                        .await
                        .expect_err("AC4: truncated drained sealed seg 0 must fail closed");
                    assert_rejected(
                        err,
                        &format!("AC4 ({lane_label}, {trunc_label})"),
                        tx_before,
                        calls_before,
                        hw_drained,
                        &tree_before,
                    );
                    assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_drained);
                }
            }

            // =================================================================
            // AC5: Missing `events-00000000.log`.
            // =================================================================
            {
                fs::remove_file(&seg0_path).unwrap();
                let tree_before = snapshot_queue_tree_full(&init.paths.root);
                let tx_before = store.write_transaction_count();
                let calls_before = spy.recorded_calls().len();
                let err = drain_capture_queue(repo.path(), &spy)
                    .await
                    .expect_err("AC5: missing drained sealed seg 0 must fail closed");
                assert_rejected(
                    err,
                    &format!("AC5 ({lane_label}, missing-seg0)"),
                    tx_before,
                    calls_before,
                    hw_drained,
                    &tree_before,
                );
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_drained);
            }

            // =================================================================
            // AC6: Out-of-bounds DB highwater `hw_offset > LTM2.sealed[0].sealed_len`.
            // =================================================================
            {
                write_bytes_0600(&seg0_path, &orig_seg0);

                // Also directly confirm `compute_next_highwater` rejects OOB offsets.
                let oob_hw_unit = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: sealed_len + 1,
                };
                let unit_err = compute_next_highwater(oob_hw_unit, &rot_out.meta, sealed_len + 1)
                    .expect_err("compute_next_highwater must reject hw_offset > sealed_len");
                assert_corrupt_committed_frame(unit_err, "AC6 compute_next_highwater OOB guard");

                for delta in [1u64, 64u64] {
                    let oob_hw = CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: sealed_len + delta,
                    };
                    let _ = store
                        .append_capture_batch(&[], Some((lane, oob_hw)))
                        .await
                        .expect("seed OOB DB highwater for AC6");
                    assert_eq!(store.get_queue_highwater(lane).await.unwrap(), oob_hw);

                    let tree_before = snapshot_queue_tree_full(&init.paths.root);
                    let tx_before = store.write_transaction_count();
                    let calls_before = spy.recorded_calls().len();
                    let err = drain_capture_queue(repo.path(), &spy)
                        .await
                        .expect_err("AC6: hw_offset > sealed_len(S) must fail closed");
                    let err_msg = format!("{err}");
                    assert!(
                        err_msg.contains("sealed_len"),
                        "AC6 ({lane_label}, +{delta}): expected sealed_len diagnostic, got {err_msg}"
                    );
                    assert_rejected(
                        err,
                        &format!("AC6 ({lane_label}, oob+{delta})"),
                        tx_before,
                        calls_before,
                        oob_hw,
                        &tree_before,
                    );
                    assert_eq!(
                        store.get_queue_highwater(lane).await.unwrap(),
                        oob_hw,
                        "AC6 ({lane_label}, +{delta}): DB highwater must NOT advance past OOB value"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn consumer_drained_rotated_empty_advance_crash_before_and_after_commit() {
        // `RB-09j` AC1 & AC2 (`ADR-RB-05` #4 ② (c)):
        // Verify on BOTH `normal/` and `priority/` lanes:
        // - AC1 (Crash BEFORE empty-advance commit): Fault-inject consumer after
        //   authenticating `LTM2.sealed[S]` and computing `(S + 1, 0)` but strictly
        //   BEFORE `append_capture_batch(&[], (S + 1, 0))` commits (`BeforeEmptyAdvanceCommit`).
        //   Verify 0 DB tx, DB highwater & mirror still at `(S, sealed_len(S))`,
        //   and `seg S` untouched. Restart observes `(S, sealed_len(S))` and
        //   idempotently commits `(S + 1, 0)` with 0 redundant rows.
        // - AC2 (Crash AFTER empty-advance commit, BEFORE external mirror update):
        //   Fault-inject `AfterDbCommitBeforeMirror` right after
        //   `append_capture_batch(&[], (S + 1, 0))` succeeds. Verify DB highwater
        //   is already `(S + 1, 0)` while external `highwater` mirror remains stale
        //   at `(S, sealed_len(S))`. Cover BOTH:
        //     (2a) empty `S + 1` (0 new frames on restart -> 0 extra DB tx, reclaims
        //          `S`, and rewrites stale external mirror from authoritative DB `(S + 1, 0)`),
        //     (2b) non-empty `S + 1` (restart starts immediately from authoritative DB
        //          `(S + 1, 0)` with 0 duplicate empty-advance tx, ingests `S + 1`
        //          frames, and updates external mirror to `(S + 1, committed_len(S + 1))`).
        use std::io::Write as _;

        let row_count_for = |store: RedbCaptureStore, lane: CaptureQueueLane| async move {
            match lane {
                CaptureQueueLane::Normal => store
                    .list_audit_logs_by_checkpoint(TEST_AUDIT_CHECKPOINT_ID)
                    .await
                    .unwrap()
                    .len(),
                CaptureQueueLane::Priority => {
                    store.list_sessions_by_started_desc().await.unwrap().len()
                }
            }
        };

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let payload_for = |tag: &str, seq: i64| match lane {
                CaptureQueueLane::Priority => {
                    make_session_payload(tag, CaptureSessionState::Stopped, 1_700_130_000 + seq)
                }
                CaptureQueueLane::Normal => {
                    make_audit_payload(tag, &format!("2026-11-24T12:00:{:02}Z", seq % 60))
                }
            };

            // =================================================================
            // AC1: Crash AFTER `LTM2.sealed[0]` auth, BEFORE empty-advance commit.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let mut sealed_len = 0u64;
                for i in 0..3usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09j:ac1:{lane_label}:s0:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09j-ac1-{lane_label}-s0-{i}"), i as i64),
                    )
                    .unwrap();
                    sealed_len = out.meta.committed_len;
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                let hw_before = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: sealed_len,
                };
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_before);
                let mirror_before = read_highwater_mirror(&init.paths.root).unwrap().unwrap();
                assert_eq!(mirror_before.get(lane), hw_before);

                // Rotate `seg 0 -> empty seg 1` via Case C Step C2.
                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09j:ac1:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09j-ac1-{lane_label}-half"), 99),
                )
                .unwrap();
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }
                let rec = crate::storage::queue::recover_lane(repo.path(), lane).unwrap();
                assert_eq!(rec.meta.active_seg, 1);
                assert_eq!(rec.meta.sealed[0].sealed_len, sealed_len);

                // Inject crash right BEFORE `append_capture_batch(&[], (1, 0))`!
                let crash_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let tx_before_crash = store.write_transaction_count();
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut crash_obs =
                    WriterObserver::with_crash_point(WriterCrashPoint::BeforeEmptyAdvanceCommit);
                let err = drain_locked_at_root(
                    &init.paths.root,
                    &crash_spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("AC1: injected BeforeEmptyAdvanceCommit crash must fail");
                assert!(matches!(err, StorageBackendError::Io { .. }));
                drop(lock);

                // Verify post-crash state: 0 DB tx, DB hw & mirror STILL `(0, sealed_len)`, `seg 0` intact!
                assert_eq!(store.write_transaction_count(), tx_before_crash);
                assert!(crash_spy.recorded_calls().is_empty());
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_before);
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    hw_before
                );
                assert!(seg0_path.exists());
                assert_eq!(
                    read_converged_lane_tail_meta(&lane_dir)
                        .unwrap()
                        .min_live_seg,
                    0
                );

                // Restart consumer: observes `(0, sealed_len)` and idempotently commits `(1, 0)`!
                let restart_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &restart_spy)
                    .await
                    .expect("AC1: restart after BeforeEmptyAdvanceCommit must converge to (1, 0)");
                assert_eq!(rep.applied_frames, 0);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);

                let calls = restart_spy.recorded_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].item_count, 0);
                assert_eq!(calls[0].applied_count, 0);
                assert_eq!(calls[0].tx_delta, 1);
                assert_eq!(
                    calls[0].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                let expected_hw = CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: 0,
                };
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), expected_hw);
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_hw
                );
                assert_eq!(row_count_for(store.clone(), lane).await, 3);
                assert!(!seg0_path.exists());
            }

            // =================================================================
            // AC2a: Crash AFTER empty-advance commit, BEFORE external mirror update
            // (with empty `S + 1 = 1` -> restart rewrites stale mirror from DB `(1, 0)`
            // with 0 extra DB write tx and reclaims `seg 0`).
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let mut sealed_len = 0u64;
                for i in 0..2usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09j:ac2a:{lane_label}:s0:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09j-ac2a-{lane_label}-s0-{i}"), i as i64),
                    )
                    .unwrap();
                    sealed_len = out.meta.committed_len;
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                let hw_stale = CaptureQueueHighwater {
                    segment_id: 0,
                    byte_offset: sealed_len,
                };

                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09j:ac2a:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09j-ac2a-{lane_label}-half"), 99),
                )
                .unwrap();
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }
                let _ = crate::storage::queue::recover_lane(repo.path(), lane).unwrap();

                // Inject crash right AFTER DB commit of `(1, 0)` and BEFORE `write_highwater_mirror`!
                let crash_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut crash_obs =
                    WriterObserver::with_crash_point(WriterCrashPoint::AfterDbCommitBeforeMirror);
                let err = drain_locked_at_root(
                    &init.paths.root,
                    &crash_spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("AC2a: injected AfterDbCommitBeforeMirror crash must fail");
                assert!(matches!(err, StorageBackendError::Io { .. }));
                drop(lock);

                // Verify DB already committed `(1, 0)` while external mirror is STILL stale `(0, sealed_len)`!
                let hw_committed = CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: 0,
                };
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_committed);
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    hw_stale,
                    "AC2a ({lane_label}): external mirror must still be stale before restart"
                );
                assert_eq!(crash_spy.recorded_calls().len(), 1);
                assert_eq!(crash_spy.recorded_calls()[0].item_count, 0);

                // Restart consumer: MUST NOT run another DB write tx; MUST rewrite
                // external mirror to authoritative DB `(1, 0)` and reclaim `seg 0`!
                let tx_before_restart = store.write_transaction_count();
                let restart_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &restart_spy).await.expect(
                    "AC2a: restart after AfterDbCommitBeforeMirror must heal external mirror",
                );
                assert_eq!(store.write_transaction_count(), tx_before_restart);
                assert!(restart_spy.recorded_calls().is_empty());
                assert_eq!(rep.applied_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);
                assert_eq!(store.get_queue_highwater(lane).await.unwrap(), hw_committed);
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    hw_committed,
                    "AC2a ({lane_label}): restart must rewrite external mirror from authoritative DB (1, 0)"
                );
                assert!(!seg0_path.exists());
                assert_eq!(row_count_for(store.clone(), lane).await, 2);
            }

            // =================================================================
            // AC2b: Crash AFTER empty-advance commit, BEFORE external mirror update
            // when `S + 1 = 1` already carries 2 frames -> restart starts directly
            // at DB `(1, 0)` (no duplicate empty advance!), ingests `S + 1`, and
            // syncs external mirror to `(1, seg1_len)`.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                let out0 = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09j:ac2b:{lane_label}:s0:0"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09j-ac2b-{lane_label}-s0-0"), 0),
                )
                .unwrap();
                let sealed_len = out0.meta.committed_len;
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();

                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09j:ac2b:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &payload_for(&format!("rb09j-ac2b-{lane_label}-half"), 99),
                )
                .unwrap();
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(lane_dir.join(segment_log_file_name(0)))
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }
                let mut seg1_len = 0u64;
                for i in 0..2usize {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09j:ac2b:{lane_label}:s1:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload_for(&format!("rb09j-ac2b-{lane_label}-s1-{i}"), 10 + i as i64),
                    )
                    .unwrap();
                    assert_eq!(out.meta.active_seg, 1);
                    seg1_len = out.meta.committed_len;
                }

                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut crash_obs =
                    WriterObserver::with_crash_point(WriterCrashPoint::AfterDbCommitBeforeMirror);
                let _ = drain_locked_at_root(
                    &init.paths.root,
                    &setup_spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("AC2b: must crash after empty-advance commit before mirror update");
                drop(lock);

                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: sealed_len,
                    }
                );

                let restart_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep = drain_capture_queue(repo.path(), &restart_spy)
                    .await
                    .expect("AC2b: restart must drain seg 1 directly from DB (1, 0)");
                assert_eq!(rep.applied_frames, 2);
                assert_eq!(rep.deduplicated_frames, 0);
                assert_eq!(rep.reclaimed_segments, 1);
                let calls = restart_spy.recorded_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].item_count, 2);
                let expected_final = CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: seg1_len,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_final
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_final
                );
                assert_eq!(row_count_for(store.clone(), lane).await, 3);
            }
        }
    }

    #[tokio::test]
    async fn consumer_drained_rotated_s1_existing_and_appended_drain() {
        // `RB-09j` AC3, AC4 & AC5 (`ADR-RB-05` #4 ② (c)):
        // Verify on BOTH `normal/` and `priority/` lanes AND across BOTH rotation
        // paths (Case C Step C2 AND genuine 16 MiB regular rotation):
        // - AC3: After empty-advance commits `(S + 1, 0)`, the consumer immediately
        //   switches its read cursor to `(S + 1, 0)` and persists 100% of all frames
        //   that ALREADY existed in `S + 1` at rotation time (verified field-for-field).
        // - AC4: Subsequent newly appended frames written to active `S + 1` are
        //   100% persisted into `capture.redb` (verified field-for-field, 0 dups).
        // - AC5: Final DB `QUEUE_HIGHWATER_*` and external `highwater` mirror stop
        //   at exact `(S + 1, committed_len(S + 1))` — NEVER jumping to `(S + 2, 0)`
        //   and never creating `events-<S + 2>.log`.
        use std::io::Write as _;

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let sized_payload_for = |tag: &str, seq: i64, pad_bytes: usize| match lane {
                CaptureQueueLane::Priority => {
                    let mut rec = match make_session_payload(
                        tag,
                        CaptureSessionState::Stopped,
                        1_700_140_000 + seq,
                    ) {
                        CaptureBatchPayload::UpsertSession(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.metadata_json = format!(r#"{{"pad":"{}"}}"#, "q".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::UpsertSession(rec)
                }
                CaptureQueueLane::Normal => {
                    let mut rec = match make_audit_payload(
                        tag,
                        &format!("2026-11-24T13:00:{:02}Z", seq % 60),
                    ) {
                        CaptureBatchPayload::AppendAuditLog(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.justification = Some("r".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::AppendAuditLog(rec)
                }
            };

            for use_16mib_rotation in [false, true] {
                let path_tag = if use_16mib_rotation {
                    "16mib"
                } else {
                    "stepc2"
                };
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                // 1. Fill & drain active `S = 0` so DB `hw` sits at `(0, seg0_len)`.
                let pre_count = if use_16mib_rotation { 5usize } else { 3usize };
                let pad_len = if use_16mib_rotation {
                    3 * 1024 * 1024
                } else {
                    0
                };
                let mut seg0_len = 0u64;
                for i in 0..pre_count {
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09j:s1:{path_tag}:{lane_label}:s0:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09j-s1-{path_tag}-{lane_label}-s0-{i}"),
                            i as i64,
                            pad_len,
                        ),
                    )
                    .unwrap();
                    assert_eq!(out.meta.active_seg, 0);
                    seg0_len = out.meta.committed_len;
                }
                let pre_rep = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();
                assert_eq!(pre_rep.applied_frames, pre_count);
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: seg0_len,
                    }
                );

                // 2. Rotate `S = 0 -> S + 1 = 1` AND write 4 existing frames into `S + 1`.
                let mut existing_s1_payloads = Vec::with_capacity(4);
                let mut existing_s1_dedups = Vec::with_capacity(4);
                let mut existing_s1_end = 0u64;

                if !use_16mib_rotation {
                    let half = encode_postcard_frame(
                        flags,
                        &format!("rb09j:s1:{path_tag}:{lane_label}:half"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09j-s1-{path_tag}-{lane_label}-half"),
                            999,
                            0,
                        ),
                    )
                    .unwrap();
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(lane_dir.join(segment_log_file_name(0)))
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }

                for i in 0..4usize {
                    let dedup = format!("rb09j:s1:{path_tag}:{lane_label}:exist:{i}");
                    let first_rot_pad = if use_16mib_rotation && i == 0 {
                        2 * 1024 * 1024
                    } else {
                        0
                    };
                    let payload = sized_payload_for(
                        &format!("rb09j-s1-{path_tag}-{lane_label}-exist-{i}"),
                        100 + i as i64,
                        first_rot_pad,
                    );
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .expect("rotate into seg 1 and populate existing S+1 frames");
                    assert_eq!(out.meta.active_seg, 1);
                    existing_s1_end = out.meta.committed_len;
                    existing_s1_dedups.push(dedup);
                    existing_s1_payloads.push(payload);
                }

                // 3. AC3: Consumer runs empty-advance `(1, 0)` then IMMEDIATELY switches
                //    read cursor to `(1, 0)` and drains all 4 existing frames of `S + 1`!
                let drain1_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep1 = drain_capture_queue(repo.path(), &drain1_spy)
                    .await
                    .expect("AC3: empty-advance + existing S+1 frames drain must succeed");
                assert_eq!(rep1.applied_frames, 4);
                assert_eq!(rep1.deduplicated_frames, 0);
                assert_eq!(rep1.reclaimed_segments, 1);
                let calls1 = drain1_spy.recorded_calls();
                assert_eq!(calls1.len(), 2);
                assert_eq!(calls1[0].item_count, 0);
                assert_eq!(
                    calls1[0].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    }
                );
                assert_eq!(calls1[1].item_count, 4);
                assert_eq!(calls1[1].dedup_keys, existing_s1_dedups);
                assert_eq!(
                    calls1[1].db_highwater_after,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: existing_s1_end,
                    }
                );

                // 4. AC4: Append 5 subsequent new frames into active `S + 1 = 1` and
                //    verify 100% seamless ingestion from `(1, existing_s1_end)`!
                let mut appended_s1_payloads = Vec::with_capacity(5);
                let mut appended_s1_dedups = Vec::with_capacity(5);
                let mut cursor = existing_s1_end;
                for i in 0..5usize {
                    let dedup = format!("rb09j:s1:{path_tag}:{lane_label}:append:{i}");
                    let payload = sized_payload_for(
                        &format!("rb09j-s1-{path_tag}-{lane_label}-append-{i}"),
                        200 + i as i64,
                        0,
                    );
                    let out = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &dedup,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                    )
                    .expect("AC4: subsequent append into active S+1 must succeed");
                    assert_eq!(out.meta.active_seg, 1);
                    assert_eq!(out.frame_offset, cursor);
                    cursor = out.meta.committed_len;
                    appended_s1_dedups.push(dedup);
                    appended_s1_payloads.push(payload);
                }
                let final_s1_committed_len = cursor;

                let drain2_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());
                let rep2 = drain_capture_queue(repo.path(), &drain2_spy)
                    .await
                    .expect("AC4: drain of subsequently appended S+1 frames must succeed");
                assert_eq!(rep2.applied_frames, 5);
                assert_eq!(rep2.deduplicated_frames, 0);
                let calls2 = drain2_spy.recorded_calls();
                assert_eq!(calls2.len(), 1);
                assert_eq!(calls2[0].dedup_keys, appended_s1_dedups);

                // 5. AC5: Final DB `QUEUE_HIGHWATER_*` and external mirror stop at EXACT
                //    `(1, final_s1_committed_len)` — NEVER jumping to `(2, 0)`!
                let expected_final_hw = CaptureQueueHighwater {
                    segment_id: 1,
                    byte_offset: final_s1_committed_len,
                };
                assert_eq!(
                    store.get_queue_highwater(lane).await.unwrap(),
                    expected_final_hw,
                    "AC5 ({path_tag}, {lane_label}): DB highwater must stop at (1, committed_len(1)), NEVER (2, 0)"
                );
                assert_eq!(
                    read_highwater_mirror(&init.paths.root)
                        .unwrap()
                        .unwrap()
                        .get(lane),
                    expected_final_hw
                );
                assert!(!lane_dir.join(segment_log_file_name(0)).exists());
                assert!(lane_dir.join(segment_log_file_name(1)).exists());
                assert!(!lane_dir.join(segment_log_file_name(2)).exists());

                // Verify 100% payload equality for all existing + appended `S + 1` frames.
                for expected in existing_s1_payloads
                    .iter()
                    .chain(appended_s1_payloads.iter())
                {
                    match expected {
                        CaptureBatchPayload::AppendAuditLog(rec) => {
                            let got = store.get_audit_log(&rec.audit_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        CaptureBatchPayload::UpsertSession(rec) => {
                            let got = store.get_session(&rec.session_id).await.unwrap().unwrap();
                            assert_eq!(&got, rec);
                        }
                        other => panic!("unexpected payload: {other:?}"),
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn consumer_drained_rotated_old_s_safe_reclaim_ordered_actions() {
        // `RB-09k` AC1–AC4 (`ADR-RB-05` #4 ② (c), ③):
        // Verify on BOTH `normal/` and `priority/` lanes after an atomic zero-frame
        // cross-segment advance commits DB `QUEUE_HIGHWATER_* = (S + 1, 0)`:
        // - Part 1 (Fault-injected mid-reclaim proof via `AfterReclaimMetaBeforeUnlink`):
        //   * Right after the empty-advance DB tx commits `(1, 0)`, `seg 0` is STILL
        //     live (`min_live_seg == 0`, `sealed == [0]`) before reclamation starts.
        //   * Reclamation acquires `append.lock` under `writer.lock` and atomically
        //     commits BOTH `tail.meta` and `tail.meta.bak` with `MIN_LIVE_SEG = 1`
        //     (AC1) AND `sealed[0]` removed in that same dual `LTM2` commit (AC2)
        //     STRICTLY BEFORE `unlink("events-00000000.log")` executes (proven by
        //     `seg 0` still existing on disk when crashing right after dual `LTM2`).
        //   * Restart cleans the post-LTM2 crash orphan with 0 quarantine and 0 dups.
        // - Part 2 (Full ordered-action proof across Case C Step C2, real 16 MiB
        //   regular rotation, AND non-zero `S = 1 -> 2`):
        //   * Lock order `writer.lock -> append.lock` proven via `with_before_reclaim_hook`.
        //   * Exact `SegmentOpEvent` durability order:
        //     `AcquireAppendLock < MetaRename(tail.meta) < FsyncLaneDirAfterMeta(tail.meta)
        //      < MetaRename(tail.meta.bak) < FsyncLaneDirAfterMeta(tail.meta.bak)
        //      < ReclaimSegmentUnlink(S) < FsyncLaneDirAfterReclaim(S) < ReleaseAppendLock`
        //     proving AC1 (MIN_LIVE_SEG = S + 1 before unlink), AC2 (sealed[S] removed
        //     in same dual LTM2), AC3 (events-<S>.log unlinked after dual LTM2), and
        //     AC4 (`fsync_dir(lane_dir)` immediately after unlink).
        use std::io::Write as _;

        let assert_ordered_reclaim_events =
            |events: &[SegmentOpEvent], lane_label: &'static str, seg_id: u32, tag: &str| {
                let pos_lock = events
                    .iter()
                    .position(|e| *e == SegmentOpEvent::AcquireAppendLock)
                    .unwrap_or_else(|| panic!("{tag}: missing AcquireAppendLock"));
                let pos_meta_primary = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::MetaRename {
                            lane: lane_label,
                            tmp_name: QUEUE_TAIL_META_TMP_FILE,
                            target_name: QUEUE_TAIL_META_FILE,
                        }
                    })
                    .unwrap_or_else(|| panic!("{tag}: missing MetaRename(tail.meta)"));
                let pos_fsync_primary = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                            lane: lane_label,
                            target_name: QUEUE_TAIL_META_FILE,
                        }
                    })
                    .unwrap_or_else(|| panic!("{tag}: missing FsyncLaneDirAfterMeta(tail.meta)"));
                let pos_meta_bak = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::MetaRename {
                            lane: lane_label,
                            tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                            target_name: QUEUE_TAIL_META_BAK_FILE,
                        }
                    })
                    .unwrap_or_else(|| panic!("{tag}: missing MetaRename(tail.meta.bak)"));
                let pos_fsync_bak = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                            lane: lane_label,
                            target_name: QUEUE_TAIL_META_BAK_FILE,
                        }
                    })
                    .unwrap_or_else(|| {
                        panic!("{tag}: missing FsyncLaneDirAfterMeta(tail.meta.bak)")
                    });
                let pos_unlink = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::ReclaimSegmentUnlink {
                            lane: lane_label,
                            seg_id,
                        }
                    })
                    .unwrap_or_else(|| panic!("{tag}: missing ReclaimSegmentUnlink({seg_id})"));
                let pos_fsync_reclaim = events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterReclaim {
                            lane: lane_label,
                            seg_id,
                        }
                    })
                    .unwrap_or_else(|| panic!("{tag}: missing FsyncLaneDirAfterReclaim({seg_id})"));
                let pos_unlock = events
                    .iter()
                    .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
                    .unwrap_or_else(|| panic!("{tag}: missing ReleaseAppendLock"));
                assert!(
                    pos_lock < pos_meta_primary
                        && pos_meta_primary < pos_fsync_primary
                        && pos_fsync_primary < pos_meta_bak
                        && pos_meta_bak < pos_fsync_bak
                        && pos_fsync_bak < pos_unlink
                        && pos_fsync_reclaim == pos_unlink + 1
                        && pos_fsync_reclaim < pos_unlock,
                    "{tag}: invalid reclaim action order (fsync_dir must immediately follow unlink); events={events:?}"
                );
            };

        for lane in [CaptureQueueLane::Normal, CaptureQueueLane::Priority] {
            let lane_label = lane_subdir_name(lane);
            let flags = match lane {
                CaptureQueueLane::Priority => FRAME_FLAG_PRIORITY,
                CaptureQueueLane::Normal => FRAME_FLAG_NONE,
            };
            let sized_payload_for = |tag: &str, seq: i64, pad_bytes: usize| match lane {
                CaptureQueueLane::Priority => {
                    let mut rec = match make_session_payload(
                        tag,
                        CaptureSessionState::Stopped,
                        1_700_150_000 + seq,
                    ) {
                        CaptureBatchPayload::UpsertSession(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.metadata_json = format!(r#"{{"pad":"{}"}}"#, "k".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::UpsertSession(rec)
                }
                CaptureQueueLane::Normal => {
                    let mut rec = match make_audit_payload(
                        tag,
                        &format!("2026-11-24T14:00:{:02}Z", seq % 60),
                    ) {
                        CaptureBatchPayload::AppendAuditLog(r) => r,
                        _ => unreachable!(),
                    };
                    if pad_bytes > 0 {
                        rec.justification = Some("k".repeat(pad_bytes));
                    }
                    CaptureBatchPayload::AppendAuditLog(rec)
                }
            };

            // =================================================================
            // Part 1: Fault-injected `AfterReclaimMetaBeforeUnlink` right after
            // empty-advance `(0, sealed_len) -> (1, 0)` proves AC1 & AC2 happen
            // atomically in dual `LTM2` strictly BEFORE `unlink(events-00000000.log)`.
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                for i in 0..2usize {
                    let _ = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09k:p1:{lane_label}:s0:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(&format!("rb09k-p1-{lane_label}-s0-{i}"), i as i64, 0),
                    )
                    .unwrap();
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();

                let half = encode_postcard_frame(
                    flags,
                    &format!("rb09k:p1:{lane_label}:half"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(&format!("rb09k-p1-{lane_label}-half"), 99, 0),
                )
                .unwrap();
                let seg0_path = lane_dir.join(segment_log_file_name(0));
                {
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .open(&seg0_path)
                        .unwrap();
                    f.write_all(&half[..half.len() / 2]).unwrap();
                    f.sync_data().unwrap();
                }
                let s1_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09k:p1:{lane_label}:s1:0"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(&format!("rb09k-p1-{lane_label}-s1-0"), 10, 0),
                )
                .unwrap();
                assert_eq!(s1_out.meta.active_seg, 1);
                assert_eq!(s1_out.meta.sealed.len(), 1);

                let saw_empty_advance_before_reclaim =
                    Arc::new(std::sync::atomic::AtomicBool::new(false));
                let flag_hook = Arc::clone(&saw_empty_advance_before_reclaim);
                let seg0_hook = seg0_path.clone();
                let lane_dir_hook = lane_dir.clone();

                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut crash_obs = WriterObserver::with_reclaim_crash_point(
                    SegmentCrashPoint::AfterReclaimMetaBeforeUnlink,
                );
                let spy = SpyCaptureStore::new_with_lock_tracking(
                    store.clone(),
                    init.paths.root.clone(),
                    crash_obs.consumer_holds_append_lock_handle(),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
                .with_after_batch_callback(move |call| {
                    assert_eq!(call.item_count, 0);
                    assert_eq!(
                        call.db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: 1,
                            byte_offset: 0,
                        }
                    );
                    assert!(seg0_hook.exists());
                    let m = read_converged_lane_tail_meta(&lane_dir_hook).unwrap();
                    assert_eq!(m.min_live_seg, 0);
                    assert_eq!(m.sealed.len(), 1);
                    flag_hook.store(true, std::sync::atomic::Ordering::Release);
                });

                let err = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut crash_obs,
                )
                .await
                .expect_err("Part 1: must crash after dual LTM2 reclaim commit and before unlink");
                assert!(matches!(err, StorageBackendError::Io { .. }));
                drop(lock);

                assert!(
                    saw_empty_advance_before_reclaim.load(std::sync::atomic::Ordering::Acquire)
                );
                // AC1 & AC2 committed atomically to BOTH `tail.meta` and `tail.meta.bak`
                // while `events-00000000.log` STILL exists on disk!
                assert!(seg0_path.exists());
                let raw_primary = fs::read(tail_meta_path(&lane_dir)).unwrap();
                let raw_bak = fs::read(tail_meta_bak_path(&lane_dir)).unwrap();
                assert_eq!(raw_primary, raw_bak);
                let meta_pre_unlink = LaneTailMeta::decode(&raw_primary).unwrap();
                assert_eq!(meta_pre_unlink.min_live_seg, 1);
                assert_eq!(meta_pre_unlink.active_seg, 1);
                assert!(meta_pre_unlink.sealed.is_empty());
                assert!(!crash_obs.segment_events.iter().any(|e| matches!(
                    e,
                    SegmentOpEvent::ReclaimSegmentUnlink { .. }
                        | SegmentOpEvent::FsyncLaneDirAfterReclaim { .. }
                )));

                // Clean restart removes post-LTM2 crash orphan and drains `seg 1`.
                let restart_rep = drain_capture_queue(repo.path(), &setup_spy)
                    .await
                    .expect("restart after pre-unlink crash must converge");
                assert_eq!(restart_rep.applied_frames, 1);
                assert_eq!(restart_rep.deduplicated_frames, 0);
                assert!(!seg0_path.exists());
            }

            // =================================================================
            // Part 2: Healthy ordered-action reclamation proof across:
            //   (a) Case C Step C2 rotation (`S = 0 -> 1`, then `S = 1 -> 2` non-zero S!),
            //   (b) Genuine 16 MiB regular rotation (`S = 0 -> 1`).
            // =================================================================
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                // Run two consecutive drained-then-rotated cycles (`S = 0 -> 1` and `S = 1 -> 2`).
                for seg_s in [0u32, 1u32] {
                    let _ = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09k:p2a:{lane_label}:s{seg_s}:0"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09k-p2a-{lane_label}-s{seg_s}-0"),
                            i64::from(seg_s) * 10,
                            0,
                        ),
                    )
                    .unwrap();
                    let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();

                    let half = encode_postcard_frame(
                        flags,
                        &format!("rb09k:p2a:{lane_label}:s{seg_s}:half"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09k-p2a-{lane_label}-s{seg_s}-half"),
                            90 + i64::from(seg_s),
                            0,
                        ),
                    )
                    .unwrap();
                    let old_seg_path = lane_dir.join(segment_log_file_name(seg_s));
                    {
                        let mut f = fs::OpenOptions::new()
                            .append(true)
                            .open(&old_seg_path)
                            .unwrap();
                        f.write_all(&half[..half.len() / 2]).unwrap();
                        f.sync_data().unwrap();
                    }
                    let rec = crate::storage::queue::recover_lane(repo.path(), lane).unwrap();
                    assert_eq!(rec.meta.min_live_seg, seg_s);
                    assert_eq!(rec.meta.active_seg, seg_s + 1);
                    assert_eq!(rec.meta.sealed.len(), 1);
                    assert_eq!(rec.meta.sealed[0].seg_id, seg_s);

                    // Verify `writer.lock -> append.lock` lock ordering right before reclaim.
                    let root_for_hook = init.paths.root.clone();
                    let saw_reclaim_hook = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let hook_flag = Arc::clone(&saw_reclaim_hook);

                    let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                        .unwrap()
                        .unwrap();
                    let mut obs = WriterObserver::with_before_reclaim_hook(move |l, s| {
                        assert_eq!(l, lane);
                        assert_eq!(s, seg_s);
                        assert!(is_writer_active_at_root(&root_for_hook).unwrap());
                        assert!(probe_append_lock_is_free(&root_for_hook));
                        hook_flag.store(true, std::sync::atomic::Ordering::Release);
                    });
                    let spy = SpyCaptureStore::new_with_lock_tracking(
                        store.clone(),
                        init.paths.root.clone(),
                        obs.consumer_holds_append_lock_handle(),
                        Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    );

                    let rep = drain_locked_at_root(
                        &init.paths.root,
                        &spy,
                        QueueWriterOptions::default(),
                        &lock,
                        &mut obs,
                    )
                    .await
                    .expect("Part 2a: empty-advance + safe ordered reclaim must succeed");
                    drop(lock);

                    assert!(saw_reclaim_hook.load(std::sync::atomic::Ordering::Acquire));
                    assert_eq!(rep.applied_frames, 0);
                    assert_eq!(rep.reclaimed_segments, 1);
                    let calls = spy.recorded_calls();
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0].item_count, 0);
                    assert_eq!(
                        calls[0].db_highwater_after,
                        CaptureQueueHighwater {
                            segment_id: seg_s + 1,
                            byte_offset: 0,
                        }
                    );

                    // AC1–AC4 strict ordered action proof + on-disk dual LTM2 & unlink check!
                    assert_ordered_reclaim_events(
                        &obs.segment_events,
                        lane_label,
                        seg_s,
                        &format!("RB-09k StepC2 ({lane_label}, S={seg_s})"),
                    );
                    assert!(
                        !old_seg_path.exists(),
                        "AC3 ({lane_label}, S={seg_s}): events-{seg_s:08}.log must be unlinked"
                    );
                    assert!(lane_dir.join(segment_log_file_name(seg_s + 1)).exists());
                    let raw_p = fs::read(tail_meta_path(&lane_dir)).unwrap();
                    let raw_b = fs::read(tail_meta_bak_path(&lane_dir)).unwrap();
                    assert_eq!(raw_p, raw_b);
                    let final_meta = LaneTailMeta::decode(&raw_p).unwrap();
                    assert_eq!(final_meta.min_live_seg, seg_s + 1);
                    assert_eq!(final_meta.active_seg, seg_s + 1);
                    assert!(final_meta.sealed.is_empty());
                }
            }

            // Part 2b: Genuine 16 MiB regular rotation (`S = 0 -> 1`) ordered reclaim proof.
            {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let lane_dir = match lane {
                    CaptureQueueLane::Normal => init.paths.normal.clone(),
                    CaptureQueueLane::Priority => init.paths.priority.clone(),
                };
                let store = RedbCaptureStore::create(
                    repo.path()
                        .join(PRIVATE_SUBDIR)
                        .join("capture.gen-0001.redb"),
                )
                .await
                .expect("create RedbCaptureStore");
                let setup_spy = SpyCaptureStore::new(store.clone(), init.paths.root.clone());

                for i in 0..5usize {
                    let _ = append_postcard_frame(
                        repo.path(),
                        lane,
                        flags,
                        &format!("rb09k:p2b:{lane_label}:big:{i}"),
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &sized_payload_for(
                            &format!("rb09k-p2b-{lane_label}-big-{i}"),
                            i as i64,
                            3 * 1024 * 1024,
                        ),
                    )
                    .unwrap();
                }
                let _ = drain_capture_queue(repo.path(), &setup_spy).await.unwrap();

                let rot_out = append_postcard_frame(
                    repo.path(),
                    lane,
                    flags,
                    &format!("rb09k:p2b:{lane_label}:rot:5"),
                    CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                    &sized_payload_for(
                        &format!("rb09k-p2b-{lane_label}-rot-5"),
                        5,
                        2 * 1024 * 1024,
                    ),
                )
                .unwrap();
                assert_eq!(rot_out.meta.active_seg, 1);

                let lock = try_acquire_writer_lock_at_root(&init.paths.root)
                    .unwrap()
                    .unwrap();
                let mut obs = WriterObserver::new();
                let spy = SpyCaptureStore::new_with_lock_tracking(
                    store.clone(),
                    init.paths.root.clone(),
                    obs.consumer_holds_append_lock_handle(),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                );
                let rep = drain_locked_at_root(
                    &init.paths.root,
                    &spy,
                    QueueWriterOptions::default(),
                    &lock,
                    &mut obs,
                )
                .await
                .expect("Part 2b: 16 MiB empty-advance + safe ordered reclaim must succeed");
                drop(lock);

                assert_eq!(rep.applied_frames, 1);
                assert_eq!(rep.reclaimed_segments, 1);
                assert_ordered_reclaim_events(
                    &obs.segment_events,
                    lane_label,
                    0,
                    &format!("RB-09k 16MiB ({lane_label})"),
                );
                assert!(!lane_dir.join(segment_log_file_name(0)).exists());
                assert!(lane_dir.join(segment_log_file_name(1)).exists());
                let raw_p = fs::read(tail_meta_path(&lane_dir)).unwrap();
                let raw_b = fs::read(tail_meta_bak_path(&lane_dir)).unwrap();
                assert_eq!(raw_p, raw_b);
                let final_meta = LaneTailMeta::decode(&raw_p).unwrap();
                assert_eq!(final_meta.min_live_seg, 1);
                assert_eq!(final_meta.active_seg, 1);
                assert!(final_meta.sealed.is_empty());
                assert!(probe_append_lock_is_free(&init.paths.root));
                assert!(!is_writer_active(repo.path()).unwrap());
            }
        }
    }
}
