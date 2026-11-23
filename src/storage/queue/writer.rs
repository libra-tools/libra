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
        segment::{
            parse_segment_log_file_name, reclaim_sealed_segment_at_root, recover_lane_at_root,
        },
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
/// queue consumer. The lock is released automatically on drop.
#[derive(Debug)]
pub struct WriterLockGuard {
    _file: File,
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
    AfterDbCommitBeforeMirror,
}

#[cfg(test)]
#[derive(Default)]
struct WriterObserver {
    crash_point: WriterCrashPoint,
    on_window_poll: Option<Box<dyn FnMut(usize) + Send>>,
    on_sealed_pre_commit: Option<Box<dyn FnMut(u32) + Send>>,
    simulated_elapsed_ms: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

#[cfg(test)]
impl WriterObserver {
    fn new() -> Self {
        Self::default()
    }

    fn with_crash_point(crash_point: WriterCrashPoint) -> Self {
        Self {
            crash_point,
            on_window_poll: None,
            on_sealed_pre_commit: None,
            simulated_elapsed_ms: None,
        }
    }

    fn with_window_poll_hook_and_clock(
        hook: impl FnMut(usize) + Send + 'static,
        simulated_elapsed_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            crash_point: WriterCrashPoint::None,
            on_window_poll: Some(Box::new(hook)),
            on_sealed_pre_commit: None,
            simulated_elapsed_ms: Some(simulated_elapsed_ms),
        }
    }

    fn with_sealed_pre_commit_hook(hook: impl FnMut(u32) + Send + 'static) -> Self {
        Self {
            crash_point: WriterCrashPoint::None,
            on_window_poll: None,
            on_sealed_pre_commit: Some(Box::new(hook)),
            simulated_elapsed_ms: None,
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
    _writer_lock: &WriterLockGuard,
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
    // order (`ADR-RB-05` #4 ③).
    if meta.min_live_seg < hw.segment_id && meta.min_live_seg < meta.active_seg {
        let _reclaim =
            reclaim_sealed_segment_at_root(queue_root, lane, meta.min_live_seg, hw.segment_id)?;
        report.reclaimed_segments += 1;
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
                let _reclaim = reclaim_sealed_segment_at_root(
                    queue_root,
                    lane,
                    hw.segment_id,
                    hw_snapshot.get(lane).segment_id,
                )?;
                report.reclaimed_segments += 1;
            }
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
            let _reclaim = reclaim_sealed_segment_at_root(
                queue_root,
                lane,
                hw.segment_id,
                committed_hw.segment_id,
            )?;
            report.reclaimed_segments += 1;
        }
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
        let _rec = recover_lane_at_root(queue_root, lane)?;
        report.recovery_delegations += 1;
        return Ok(LaneStepOutcome::Progressed);
    }

    *last_recovery_signature = None;
    Ok(LaneStepOutcome::CaughtUp)
}

/// Strictly read-only preflight validation of a lane's metadata, highwater
/// monotonicity, unconsumed sealed segments, and active committed prefix
/// BEFORE any DB write transaction or mirror update runs on either lane
/// (`RB-09b` AC2–AC5).
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

    if hw.segment_id < meta.active_seg {
        for seg_id in hw.segment_id..meta.active_seg {
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
                    _file: File::from(lock_fd),
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
                return Ok(Some(WriterLockGuard { _file: file }));
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
            FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY, SealedSegmentMeta, append_postcard_frame,
            crc32c_checksum, encode_postcard_frame, highwater::read_highwater_mirror,
            init_capture_queue, read_converged_lane_tail_meta, tail_meta_bak_path, tail_meta_path,
        },
    };

    const TEST_AUDIT_CHECKPOINT_ID: &str = "cp-rb09-normal";

    #[derive(Debug, Clone)]
    struct RecordedBatchCall {
        lane: CaptureQueueLane,
        item_count: usize,
        dedup_keys: Vec<String>,
        highwater: CaptureQueueHighwater,
    }

    /// Spy wrapper delegating every `CaptureStore` / `AgentBridgeStore` call to
    /// a real `RedbCaptureStore` while recording exact `append_capture_batch`
    /// call order and asserting `writer.lock` IS held while `append.lock` is
    /// NEVER held across DB writes (`GC-RB-04`).
    struct SpyCaptureStore {
        inner: RedbCaptureStore,
        queue_root: PathBuf,
        calls: Arc<Mutex<Vec<RecordedBatchCall>>>,
    }

    impl SpyCaptureStore {
        fn new(inner: RedbCaptureStore, queue_root: PathBuf) -> Self {
            Self {
                inner,
                queue_root,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_calls(&self) -> Vec<RecordedBatchCall> {
            self.calls.lock().expect("lock recorded calls").clone()
        }

        /// Assert `writer.lock` IS held and `capture-queue/append.lock` is NOT
        /// held by the consumer while executing `CaptureStore::append_capture_batch`.
        fn assert_lock_invariants_during_db_write(&self) {
            assert!(
                is_writer_active_at_root(&self.queue_root)
                    .expect("probe writer.lock during DB write"),
                "consumer must hold capture-queue/writer.lock during CaptureStore::append_capture_batch"
            );
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd as _;
                let lock_path = self
                    .queue_root
                    .join(crate::storage::queue::QUEUE_APPEND_LOCK_FILE);
                if lock_path.exists() {
                    let file = fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&lock_path)
                        .expect("open append.lock for non-blocking probe");
                    // SAFETY: `file` is valid.
                    let rc =
                        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                    assert_eq!(
                        rc, 0,
                        "consumer must NEVER hold capture-queue/append.lock during CaptureStore::append_capture_batch"
                    );
                }
            }
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
            self.assert_lock_invariants_during_db_write();
            let outcome = self
                .inner
                .append_capture_batch(items, highwater_advance)
                .await?;
            if let Some((lane, hw)) = highwater_advance {
                self.calls
                    .lock()
                    .expect("lock calls")
                    .push(RecordedBatchCall {
                        lane,
                        item_count: items.len(),
                        dedup_keys: items.iter().map(|i| i.dedup_key.clone()).collect(),
                        highwater: outcome.highwater.unwrap_or(hw),
                    });
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
}
