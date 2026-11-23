//! Crash-safe concurrent capture queue segment append, `LTM2` dual-replica
//! (`tail.meta` + `tail.meta.bak`) atomic commit, strict total-order replica
//! convergence, and Case C same-segment recovery (`ADR-RB-05` #3, `RB-08`).
//!
//! ## Protocol Guarantees (`RB-08`)
//! - Frame encoding and validation occur **outside** the shared cross-lane
//!   `.libra/private/capture-queue/append.lock`.
//! - Inside `append.lock`, Phase 1 executes a **100% read-only** classification
//!   of the target lane (`events-*.log`, `tail.meta`, `tail.meta.bak`, active
//!   segment committed prefix `[0, committed_len)` hardware `crc32c`, and any
//!   uncommitted Case C tail). Any fail-closed corruption state returns
//!   [`StorageBackendError::CorruptCommittedFrame`] (`LBR-IO-001`) with zero
//!   disk mutation.
//! - Phase 2 cleans stale `<lane>/tail.meta*.tmp` files, removes reclaimed
//!   orphans (`seg < min_live_seg`), converges lagging/missing dual replicas by
//!   the strict total order `(active_seg, committed_len, min_live_seg)`, and
//!   persists Case C same-segment valid synced frames before appending any new
//!   frame.
//! - Every frame append executes a single `write_all(&frame_buf)` +
//!   `seg.sync_data()`, then atomically replaces `tail.meta` via
//!   `tail.meta.tmp -> sync_data -> rename -> fsync_dir(lane_dir)` followed by
//!   `tail.meta.bak` via `tail.meta.bak.tmp -> sync_data -> rename ->
//!   fsync_dir(lane_dir)` strictly **before** releasing `append.lock`.

#[cfg(unix)]
use std::{
    ffi::{CStr, OsStr, OsString},
    os::unix::{
        ffi::OsStrExt as _,
        io::{AsRawFd as _, FromRawFd as _, OwnedFd},
    },
};
use std::{
    fs::File,
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Serialize;

#[cfg(unix)]
use crate::storage::queue::{
    QUEUE_APPEND_LOCK_FILE, QUEUE_DIR_MODE, QUEUE_FILE_MODE, UnixExpectedQueueOwners,
    unix_impl::{
        AppendLockGuard, DirStreamGuard, TempUnlinkGuard, clear_errno, fchmod_fd, fstat_fd,
        fsync_fd, is_symlink_errno_at, open_ancestor_dir, open_dir_nofollow, openat_dir_nofollow,
        osstr_to_cstring, read_bounded_fd_with_context, unix_perm_bits,
    },
};
#[cfg(windows)]
use crate::storage::queue::{
    QUEUE_APPEND_LOCK_FILE, durable_replace_publish_windows, flush_windows_dir,
};
use crate::storage::{
    capture_store::CaptureQueueLane,
    error::{StorageBackendError, StorageResult},
    manifest::PRIVATE_SUBDIR,
    queue::{
        CAPTURE_QUEUE_SUBDIR, FrameDecodeError, FrameEncodeError, FrameStreamDecoder,
        FrameStreamItem, LTM2_MAX_LEN, LaneTailMeta, Ltm2CodecError, MAX_SEALED_SEGMENTS,
        QUEUE_LANE_NORMAL_DIR, QUEUE_LANE_PRIORITY_DIR, QUEUE_QUARANTINE_DIR,
        QUEUE_TAIL_META_BAK_FILE, QUEUE_TAIL_META_FILE, SealedSegmentMeta, crc32c_append,
        crc32c_checksum, decode_frame, encode_frame, encode_postcard_frame,
        map_queue_owner_mismatch_err, map_queue_symlink_err, matches_segment_log_glob_bytes,
        segment_log_file_name,
    },
};

/// Staging filename used during atomic replacement of `tail.meta` (`ADR-RB-05` #3, `RB-08`).
pub const QUEUE_TAIL_META_TMP_FILE: &str = "tail.meta.tmp";

/// Staging filename used during atomic replacement of `tail.meta.bak` (`ADR-RB-05` #3, `RB-08`).
pub const QUEUE_TAIL_META_BAK_TMP_FILE: &str = "tail.meta.bak.tmp";

/// Stable diagnostic code recorded whenever uncommitted damaged tail or orphan
/// bytes are durably published into `quarantine/` (`ADR-RB-05` #3, `RB-08t`).
pub const QUARANTINE_DIAGNOSTIC_CODE: &str = "LBR-AGENT-050";

/// Hard combined byte cap across unconsumed `priority/`, unconsumed `normal/`,
/// `quarantine/` artifacts, and the incoming encoded frame (`ADR-RB-05` #3,
/// `RB-08b`: `256 MiB`).
pub const CAPTURE_QUEUE_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Active segment rotation threshold (`ADR-RB-05` #3 Step ③, `RB-08g`: `16 MiB`).
///
/// Whenever a non-empty active segment `S` satisfies
/// `committed_len + frame_buf.len() > CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES`,
/// the producer rotates to a fresh `events-<S+1:08>.log` via the internal
/// lock-guarded `create_segment_file` primitive and atomically seals
/// `(S, old_committed_len, old_prefix_crc32c)` into `LTM2.sealed[]`.
pub const CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES: u64 = 16 * 1024 * 1024;

/// Maximum lock wait duration enforced on shared `capture-queue/append.lock`
/// (`ADR-RB-05` #3, `RB-08b`: `min(hook_deadline, 30s)`).
pub const DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Initial polling sleep when `append.lock` is contended (exponentially backed
/// off up to [`APPEND_LOCK_MAX_BACKOFF`]).
const APPEND_LOCK_INITIAL_BACKOFF: Duration = Duration::from_micros(200);

/// Upper bound on backoff sleep between non-blocking `append.lock` retries.
const APPEND_LOCK_MAX_BACKOFF: Duration = Duration::from_millis(5);

/// Maximum legal byte length of a quarantine artifact identifier (`<lane>-seg<S>-off<offset>-<crc32c>`).
const MAX_QUARANTINE_ID_LEN: usize = 160;

/// Streaming I/O chunk size used for hardware-accelerated prefix CRC32C
/// verification and Case C incremental CRC extension (64 KiB, zero heap).
const PREFIX_CRC_CHUNK_BYTES: usize = 64 * 1024;

/// Return the canonical subdirectory name (`"priority"` or `"normal"`) for `lane`.
#[must_use]
pub(super) const fn lane_dir_name(lane: CaptureQueueLane) -> &'static str {
    match lane {
        CaptureQueueLane::Priority => QUEUE_LANE_PRIORITY_DIR,
        CaptureQueueLane::Normal => QUEUE_LANE_NORMAL_DIR,
    }
}

/// Return the sibling capture queue lane opposite to `lane`.
#[must_use]
const fn opposite_lane(lane: CaptureQueueLane) -> CaptureQueueLane {
    match lane {
        CaptureQueueLane::Priority => CaptureQueueLane::Normal,
        CaptureQueueLane::Normal => CaptureQueueLane::Priority,
    }
}

/// Returns `true` iff appending `frame_len` bytes to an active segment at
/// `committed_len` requires rotating to `active_seg + 1` (`ADR-RB-05` #3 Step ③,
/// `RB-08g`).
///
/// A 0-byte active segment (`committed_len == 0`) is never sealed/rotated.
fn should_rotate_active_segment(
    committed_len: u64,
    frame_len: usize,
    rotation_threshold_bytes: u64,
) -> StorageResult<bool> {
    if committed_len == 0 {
        return Ok(false);
    }
    let frame_len_u64 =
        u64::try_from(frame_len).map_err(|_| StorageBackendError::BackendInternal {
            detail: "frame length exceeds u64".to_string(),
        })?;
    let projected = committed_len.checked_add(frame_len_u64).ok_or_else(|| {
        StorageBackendError::BackendInternal {
            detail: "capture queue segment committed_len overflow".to_string(),
        }
    })?;
    Ok(projected > rotation_threshold_bytes)
}

/// Enforce `ADR-RB-05` #3 Step ② (b) (`RB-08j`): when an incoming frame
/// requires normal 16 MiB segment rotation (`rotating == true`) and
/// `LTM2.sealed.len() >= MAX_SEALED_SEGMENTS` (`256`), reject fail-closed with
/// recoverable backpressure `StorageBackendError::QueueFull` (`LBR-AGENT-046`)
/// strictly BEFORE creating `events-<S+1:08>.log` or modifying any segment or
/// metadata file for the incoming frame.
fn enforce_rotation_sealed_cap_limit(sealed_len: usize, rotating: bool) -> StorageResult<()> {
    if rotating && sealed_len >= MAX_SEALED_SEGMENTS {
        return Err(StorageBackendError::QueueFull {
            detail: format!(
                "capture queue sealed segment table is full ({sealed_len}/{MAX_SEALED_SEGMENTS}); reclaim sealed segments before rotating"
            ),
        });
    }
    Ok(())
}

/// Build and validate the `LTM2` metadata after reclaiming the oldest sealed
/// segment `seg_id` (`seg_id == min_live_seg && seg_id < active_seg && seg_id < highwater_seg`)
/// per `ADR-RB-05` #4 ③ (`RB-08j`, `RB-09d`).
#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
fn build_reclaimed_tail_meta(
    meta: &LaneTailMeta,
    seg_id: u32,
    highwater_seg: u32,
) -> StorageResult<LaneTailMeta> {
    if seg_id >= highwater_seg {
        return Err(StorageBackendError::BackendInternal {
            detail: "cannot reclaim capture queue segment at or beyond consumer highwater"
                .to_string(),
        });
    }
    if seg_id >= meta.active_seg {
        return Err(StorageBackendError::BackendInternal {
            detail: "cannot reclaim active or future capture queue segment".to_string(),
        });
    }
    if seg_id != meta.min_live_seg {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture queue segment reclamation must advance min_live_seg strictly in order"
                .to_string(),
        });
    }
    let Some(oldest) = meta.sealed.first() else {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "LTM2 sealed table is empty while seg_id < active_seg".to_string(),
        });
    };
    if oldest.seg_id != seg_id {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "LTM2 oldest sealed entry seg_id does not match min_live_seg".to_string(),
        });
    }
    let next_min_live =
        seg_id
            .checked_add(1)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "capture queue min_live_seg u32 overflow during reclaim".to_string(),
            })?;
    let next_meta = LaneTailMeta {
        min_live_seg: next_min_live,
        active_seg: meta.active_seg,
        committed_len: meta.committed_len,
        prefix_crc32c: meta.prefix_crc32c,
        sealed: meta.sealed[1..].to_vec(),
    };
    next_meta
        .validate()
        .map_err(Ltm2CodecError::into_storage_error)?;
    Ok(next_meta)
}

/// Validate `ADR-RB-05` #4 ③ reclamation preconditions strictly against the
/// read-only Phase 1 authoritative `LTM2` BEFORE any Phase 2 metadata repair,
/// `.tmp` cleanup, or segment recovery mutates disk state.
#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
fn validate_reclaim_plan_readonly<T>(
    plan: &Phase1Plan<T>,
    seg_id: u32,
    highwater_seg: u32,
) -> StorageResult<()> {
    let meta = match plan {
        Phase1Plan::Case0aFreshEmpty { .. } | Phase1Plan::Case0bPreInitializedEmpty { .. } => {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot reclaim sealed segment from empty capture queue lane".to_string(),
            });
        }
        Phase1Plan::CaseAClean { meta, .. }
        | Phase1Plan::CaseCRecoverTail { meta, .. }
        | Phase1Plan::CaseDRecoverValidSuccessor { meta, .. }
        | Phase1Plan::CaseDCleanOrphanSuccessor { meta, .. } => meta,
    };
    let _ = build_reclaimed_tail_meta(meta, seg_id, highwater_seg)?;
    Ok(())
}

/// Build the post-append `LTM2` metadata for either in-place append on
/// `meta.active_seg` or 16 MiB normal rotation to `meta.active_seg + 1`
/// (`ADR-RB-05` #3 Step ③–④, `RB-08g`, `RB-08j`).
fn build_next_append_meta(
    prev_meta: LaneTailMeta,
    frame_buf: &[u8],
    rotating: bool,
) -> StorageResult<(u32, u64, LaneTailMeta)> {
    let frame_len_u64 =
        u64::try_from(frame_buf.len()).map_err(|_| StorageBackendError::BackendInternal {
            detail: "frame length exceeds u64".to_string(),
        })?;
    if rotating {
        enforce_rotation_sealed_cap_limit(prev_meta.sealed.len(), true)?;
        if prev_meta.committed_len == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot seal a 0-byte capture queue segment during rotation".to_string(),
            });
        }
        let next_seg = prev_meta.active_seg.checked_add(1).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "capture queue active_seg u32 overflow during rotation".to_string(),
            }
        })?;
        let mut next_sealed = prev_meta.sealed;
        next_sealed.push(SealedSegmentMeta {
            seg_id: prev_meta.active_seg,
            sealed_len: prev_meta.committed_len,
            sealed_crc32c: prev_meta.prefix_crc32c,
        });
        let next_meta = LaneTailMeta {
            min_live_seg: prev_meta.min_live_seg,
            active_seg: next_seg,
            committed_len: frame_len_u64,
            prefix_crc32c: crc32c_append(0, frame_buf),
            sealed: next_sealed,
        };
        next_meta
            .validate()
            .map_err(Ltm2CodecError::into_storage_error)?;
        Ok((next_seg, 0, next_meta))
    } else {
        let frame_offset = prev_meta.committed_len;
        let next_committed_len = frame_offset.checked_add(frame_len_u64).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "capture queue segment committed_len overflow".to_string(),
            }
        })?;
        let next_meta = LaneTailMeta {
            min_live_seg: prev_meta.min_live_seg,
            active_seg: prev_meta.active_seg,
            committed_len: next_committed_len,
            prefix_crc32c: crc32c_append(prev_meta.prefix_crc32c, frame_buf),
            sealed: prev_meta.sealed,
        };
        next_meta
            .validate()
            .map_err(Ltm2CodecError::into_storage_error)?;
        Ok((prev_meta.active_seg, frame_offset, next_meta))
    }
}

const fn clamp_append_lock_timeout(timeout: Duration) -> Duration {
    let max_secs = DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT.as_secs();
    let max_nanos = DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT.subsec_nanos();
    let secs = timeout.as_secs();
    let nanos = timeout.subsec_nanos();
    if secs > max_secs || (secs == max_secs && nanos > max_nanos) {
        DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT
    } else {
        timeout
    }
}

/// Tunable bounds for capture-queue append operations (`ADR-RB-05` #3, `RB-08b`).
///
/// Effective bounds are always clamped so callers cannot weaken the hard
/// `256 MiB` capacity ceiling or the `30s` lock wait ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueAppendOptions {
    pub max_total_bytes: u64,
    pub lock_timeout: Duration,
}

impl Default for QueueAppendOptions {
    fn default() -> Self {
        Self {
            max_total_bytes: CAPTURE_QUEUE_MAX_TOTAL_BYTES,
            lock_timeout: DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT,
        }
    }
}

impl QueueAppendOptions {
    /// Set the combined cross-lane capacity cap (clamped to
    /// [`CAPTURE_QUEUE_MAX_TOTAL_BYTES`]).
    #[must_use]
    pub const fn with_max_total_bytes(mut self, max_total_bytes: u64) -> Self {
        self.max_total_bytes = if max_total_bytes < CAPTURE_QUEUE_MAX_TOTAL_BYTES {
            max_total_bytes
        } else {
            CAPTURE_QUEUE_MAX_TOTAL_BYTES
        };
        self
    }

    /// Set the shared `append.lock` wait deadline (clamped to
    /// [`DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT`]).
    #[must_use]
    pub const fn with_lock_timeout(mut self, lock_timeout: Duration) -> Self {
        self.lock_timeout = clamp_append_lock_timeout(lock_timeout);
        self
    }

    /// Effective combined capacity cap (`<= CAPTURE_QUEUE_MAX_TOTAL_BYTES`).
    #[must_use]
    pub const fn effective_max_total_bytes(&self) -> u64 {
        if self.max_total_bytes < CAPTURE_QUEUE_MAX_TOTAL_BYTES {
            self.max_total_bytes
        } else {
            CAPTURE_QUEUE_MAX_TOTAL_BYTES
        }
    }

    /// Effective `append.lock` wait timeout (`<= DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT`).
    #[must_use]
    pub const fn effective_lock_timeout(&self) -> Duration {
        clamp_append_lock_timeout(self.lock_timeout)
    }
}

/// Cross-lane physical byte occupancy snapshot across unconsumed `priority/`
/// segments, unconsumed `normal/` segments, and `quarantine/` artifacts
/// (`ADR-RB-05` #3, `RB-08b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureQueueOccupancy {
    pub priority_bytes: u64,
    pub normal_bytes: u64,
    pub quarantine_bytes: u64,
    pub total_bytes: u64,
}

impl CaptureQueueOccupancy {
    fn from_parts(
        priority_bytes: u64,
        normal_bytes: u64,
        quarantine_bytes: u64,
    ) -> StorageResult<Self> {
        let total_bytes = priority_bytes
            .checked_add(normal_bytes)
            .and_then(|sum| sum.checked_add(quarantine_bytes))
            .ok_or_else(|| StorageBackendError::QueueFull {
                detail: "capture queue combined occupancy overflowed u64".to_string(),
            })?;
        Ok(Self {
            priority_bytes,
            normal_bytes,
            quarantine_bytes,
            total_bytes,
        })
    }
}

fn lock_deadline_for_timeout(timeout: Duration) -> Instant {
    let clamped = clamp_append_lock_timeout(timeout);
    let now = Instant::now();
    now.checked_add(clamped).unwrap_or(now)
}

#[derive(Debug)]
enum TryLockOutcome {
    Acquired,
    WouldBlock,
    #[cfg(unix)]
    Retry,
    Error(io::Error),
}

/// Shared deadline-bounded non-blocking lock acquisition loop used by both Unix
/// `flock(LOCK_EX | LOCK_NB)` and Windows `File::try_lock()` (`ADR-RB-05` #3,
/// `RB-08b`).
///
/// Performs one immediate non-blocking attempt (so `Duration::ZERO` succeeds
/// when uncontended), then verifies `now_fn() < deadline` strictly BEFORE every
/// retry so a lock released after the caller's deadline can never be acquired.
fn acquire_lock_with_deadline_loop<F, N, S>(
    deadline: Instant,
    context: &'static str,
    mut try_lock: F,
    mut now_fn: N,
    mut sleep_fn: S,
) -> StorageResult<()>
where
    F: FnMut() -> TryLockOutcome,
    N: FnMut() -> Instant,
    S: FnMut(Duration),
{
    let mut backoff = APPEND_LOCK_INITIAL_BACKOFF;
    let mut first_attempt = true;
    loop {
        if !first_attempt && now_fn() >= deadline {
            return Err(StorageBackendError::QueueBusy {
                detail: "timed out waiting for capture queue append.lock".to_string(),
            });
        }
        first_attempt = false;
        match try_lock() {
            TryLockOutcome::Acquired => return Ok(()),
            #[cfg(unix)]
            TryLockOutcome::Retry => continue,
            TryLockOutcome::WouldBlock => {
                let now = now_fn();
                if now >= deadline {
                    return Err(StorageBackendError::QueueBusy {
                        detail: "timed out waiting for capture queue append.lock".to_string(),
                    });
                }
                let remaining = deadline.saturating_duration_since(now);
                sleep_fn(backoff.min(remaining));
                backoff = (backoff * 2).min(APPEND_LOCK_MAX_BACKOFF);
            }
            TryLockOutcome::Error(err) => return Err(StorageBackendError::io(context, err)),
        }
    }
}

fn enforce_queue_capacity_limit(
    occupancy: CaptureQueueOccupancy,
    frame_len: usize,
    max_total_bytes: u64,
) -> StorageResult<u64> {
    let frame_len_u64 = u64::try_from(frame_len).map_err(|_| StorageBackendError::QueueFull {
        detail: "capture queue frame length exceeds u64 capacity limit".to_string(),
    })?;
    let projected = match occupancy.total_bytes.checked_add(frame_len_u64) {
        Some(total) => total,
        None => {
            return Err(StorageBackendError::QueueFull {
                detail: format!(
                    "capture queue occupancy overflow exceeds {max_total_bytes}-byte hard limit"
                ),
            });
        }
    };
    if projected > max_total_bytes {
        return Err(StorageBackendError::QueueFull {
            detail: format!(
                "capture queue occupancy {projected} bytes (priority={}, normal={}, quarantine={}, incoming={frame_len_u64}) exceeds {max_total_bytes}-byte hard limit",
                occupancy.priority_bytes, occupancy.normal_bytes, occupancy.quarantine_bytes,
            ),
        });
    }
    Ok(projected)
}

/// Parse a canonical segment log filename (`events-<seg_id:08>.log`) into its
/// `u32` segment identifier.
///
/// Enforces exact round-trip canonicality against [`segment_log_file_name`] so
/// non-canonical or non-UTF-8 glob matches such as `events-0.log`,
/// `events-.log`, `events-000000001.log`, or `events-\xff.log` return `None`.
#[must_use]
pub fn parse_segment_log_file_name(name: &[u8]) -> Option<u32> {
    if !name.starts_with(b"events-") || !name.ends_with(b".log") {
        return None;
    }
    let digits = &name[b"events-".len()..name.len() - b".log".len()];
    if !(8..=10).contains(&digits.len()) || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits_str = std::str::from_utf8(digits).ok()?;
    let seg_id = digits_str.parse::<u32>().ok()?;
    if segment_log_file_name(seg_id).as_bytes() == name {
        Some(seg_id)
    } else {
        None
    }
}

/// Returns `true` iff `name` is a lane-local `tail.meta*.tmp` staging file
/// eligible for cleanup in Phase 2 after read-only classification confirms the
/// lane is valid/recoverable (`ADR-RB-05` #3).
#[must_use]
pub fn is_lane_meta_temp_name(name: &[u8]) -> bool {
    name == QUEUE_TAIL_META_TMP_FILE.as_bytes()
        || name == QUEUE_TAIL_META_BAK_TMP_FILE.as_bytes()
        || (name.starts_with(b"tail.meta") && name.ends_with(b".tmp"))
}

/// Construct the canonical deterministic quarantine artifact identifier from a
/// precomputed Castagnoli CRC32C checksum (used by bounded streaming recovery
/// paths without buffering the entire tail in memory).
#[must_use]
fn quarantine_artifact_id_from_crc(
    lane: CaptureQueueLane,
    seg_id: u32,
    offset: u64,
    crc32c: u32,
) -> String {
    format!(
        "{}-seg{seg_id:08}-off{offset}-{crc32c:08x}",
        lane_dir_name(lane)
    )
}

/// Construct the canonical deterministic quarantine artifact identifier
/// `<lane>-seg<S:08>-off<offset>-<crc32c:08x>` (`ADR-RB-05` #3, `RB-08t`).
///
/// The `<lane>-` prefix ensures Phase 2 lane-scoped cleanup ONLY ever removes
/// leftover `quarantine/<lane>-*.tmp` files owned by the lane being recovered,
/// never touching the opposite lane's temporary files.
#[must_use]
pub fn quarantine_artifact_id(
    lane: CaptureQueueLane,
    seg_id: u32,
    offset: u64,
    damaged_bytes: &[u8],
) -> String {
    quarantine_artifact_id_from_crc(lane, seg_id, offset, crc32c_checksum(damaged_bytes))
}

/// Returns `true` iff `name` is a lane-scoped quarantine temporary file
/// `quarantine/<lane>-*.tmp` eligible for Phase 2 cleanup ONLY when `lane` has
/// passed Phase 1 read-only classification (`ADR-RB-05` #3, `RB-08t`).
#[must_use]
pub fn is_lane_quarantine_temp_name(lane: CaptureQueueLane, name: &[u8]) -> bool {
    let prefix = match lane {
        CaptureQueueLane::Priority => b"priority-".as_slice(),
        CaptureQueueLane::Normal => b"normal-".as_slice(),
    };
    name.starts_with(prefix) && name.ends_with(b".tmp") && name.len() > prefix.len() + b".tmp".len()
}

fn validate_quarantine_artifact_id(id: &str) -> StorageResult<()> {
    if id.is_empty() || id.len() > MAX_QUARANTINE_ID_LEN {
        return Err(StorageBackendError::BackendInternal {
            detail: "quarantine artifact id length is invalid".to_string(),
        });
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "quarantine artifact id contains invalid characters".to_string(),
        });
    }
    Ok(())
}

/// Result returned after durably publishing damaged uncommitted bytes into
/// `quarantine/<id>.bin` via `.tmp -> sync_data -> rename(.bin) -> fsync_dir`
/// (`ADR-RB-05` #3, `RB-08t`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinePublishReport {
    pub id: String,
    pub bin_path: PathBuf,
    pub quarantined_bytes: u64,
    pub crc32c: u32,
    pub diagnostic_code: &'static str,
}

/// Outcome returned after a durable frame append to a capture queue lane (`RB-08`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentAppendOutcome {
    pub lane: CaptureQueueLane,
    pub seg_id: u32,
    pub frame_offset: u64,
    pub frame_len: usize,
    pub meta: LaneTailMeta,
    pub recovered_frames: usize,
}

/// Outcome returned by [`recover_lane`] / [`recover_lane_at_root`] (`RB-08`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneRecoveryReport {
    pub lane: CaptureQueueLane,
    pub meta: LaneTailMeta,
    pub recovered_frames: usize,
    pub recovered_bytes: u64,
    pub mirrored_replica: bool,
    pub cleaned_tmp_files: usize,
    pub cleaned_quarantine_tmp_files: usize,
    pub reclaimed_orphans_removed: usize,
    pub quarantined_artifacts: Vec<QuarantinePublishReport>,
}

// ============================================================================
// Internal Phase 1 / Phase 2 State Machine Types
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplicaState {
    Absent,
    Valid(LaneTailMeta),
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorSync {
    None,
    SyncPrimary,
    SyncBackup,
}

/// Result of resolving `tail.meta` and `tail.meta.bak` read-only (`ADR-RB-05` #3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedReplicas {
    /// Both `tail.meta` and `tail.meta.bak` are absent (`ENOENT`).
    BothAbsent,
    /// At least one replica is valid; `meta` is authoritative per strict total
    /// order `(active_seg, committed_len, min_live_seg)`.
    Authoritative {
        meta: LaneTailMeta,
        mirror_sync: MirrorSync,
    },
    /// At least one replica exists on disk and neither is valid `LTM2`.
    BothInvalidWithPresence,
}

fn resolve_dual_replicas(
    primary: ReplicaState,
    backup: ReplicaState,
) -> StorageResult<ResolvedReplicas> {
    match (primary, backup) {
        (ReplicaState::Valid(p), ReplicaState::Valid(b)) => {
            match p.ordering_key().cmp(&b.ordering_key()) {
                std::cmp::Ordering::Greater => Ok(ResolvedReplicas::Authoritative {
                    meta: p,
                    mirror_sync: MirrorSync::SyncBackup,
                }),
                std::cmp::Ordering::Less => Ok(ResolvedReplicas::Authoritative {
                    meta: b,
                    mirror_sync: MirrorSync::SyncPrimary,
                }),
                std::cmp::Ordering::Equal => {
                    if p != b {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail: "dual LTM2 replicas share ordering key (active_seg, committed_len, min_live_seg) but disagree on prefix_crc32c or sealed table"
                                .to_string(),
                        });
                    }
                    Ok(ResolvedReplicas::Authoritative {
                        meta: p,
                        mirror_sync: MirrorSync::None,
                    })
                }
            }
        }
        (ReplicaState::Valid(p), ReplicaState::Absent | ReplicaState::Corrupt) => {
            Ok(ResolvedReplicas::Authoritative {
                meta: p,
                mirror_sync: MirrorSync::SyncBackup,
            })
        }
        (ReplicaState::Absent | ReplicaState::Corrupt, ReplicaState::Valid(b)) => {
            Ok(ResolvedReplicas::Authoritative {
                meta: b,
                mirror_sync: MirrorSync::SyncPrimary,
            })
        }
        (ReplicaState::Absent, ReplicaState::Absent) => Ok(ResolvedReplicas::BothAbsent),
        (ReplicaState::Corrupt, ReplicaState::Absent | ReplicaState::Corrupt)
        | (ReplicaState::Absent, ReplicaState::Corrupt) => {
            Ok(ResolvedReplicas::BothInvalidWithPresence)
        }
    }
}

/// Action plan computed during strictly read-only Phase 1 classification.
/// Read-only Phase 1 plan for a nested Case C→D old active segment `S` whose
/// on-disk size exceeds `LTM2.committed_len` (`ADR-RB-05` #3 Case D, `RB-08k`, `RB-08v`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NestedOldTailPlan {
    valid_end: u64,
    recovered_crc32c: u32,
    frame_count: usize,
    has_damaged_tail: bool,
}

#[derive(Debug)]
enum Phase1Plan<T> {
    /// Case 0a: 0 segment files and both replicas absent; initialize 34B `LTM2`
    /// `(0, 0, 0, 0, [])` dual replicas before creating `events-00000000.log`.
    Case0aFreshEmpty { stale_tmp_names: Vec<T> },
    /// Case 0b: 0 segment files and valid initial empty `LTM2` `(0, 0, 0, 0, [])`.
    Case0bPreInitializedEmpty {
        meta: LaneTailMeta,
        mirror_sync: MirrorSync,
        stale_tmp_names: Vec<T>,
    },
    /// Case A: `max_live_seg == active_seg` and `actual_len == committed_len`,
    /// with `[0, committed_len)` prefix CRC32C verified.
    CaseAClean {
        meta: LaneTailMeta,
        mirror_sync: MirrorSync,
        stale_tmp_names: Vec<T>,
        reclaimed_orphans: Vec<u32>,
    },
    /// Case C (same-segment uncommitted tail): `max_live_seg == active_seg`,
    /// `actual_len > committed_len`, `[0, committed_len)` prefix CRC32C
    /// verified; `[committed_len, recovered_valid_end)` decodes `recovered_frames`
    /// complete dual-CRC32C valid frames, and if `has_damaged_tail` is `true`,
    /// `[recovered_valid_end, actual_len)` contains uncommitted damaged/truncated
    /// tail bytes requiring Step C1 durable quarantine + truncate + `sync_data()`
    /// before committing `LTM2` (`ADR-RB-05` #3 Case C Step C1, `RB-08`, `RB-08m`).
    CaseCRecoverTail {
        meta: LaneTailMeta,
        recovered_valid_end: u64,
        recovered_crc32c: u32,
        recovered_frames: usize,
        has_damaged_tail: bool,
        stale_tmp_names: Vec<T>,
        reclaimed_orphans: Vec<u32>,
    },
    /// Case D (valid successor `S + 1`, optionally preceded by a nested Case C
    /// old-segment tail `old_tail` on `S` and/or followed by an uncommitted
    /// trailing half-frame `[succ_valid_end, actual_len)` on `S + 1`):
    /// `max_live_seg == active_seg + 1`, old segment `S` `[0, committed_len)`
    /// prefix CRC32C verified, `meta.sealed.len() < MAX_SEALED_SEGMENTS`,
    /// effective old length `> 0`, and `events-<S+1>.log` decodes `>= 1` valid
    /// dual-CRC32C frame from offset `0` to `succ_valid_end` (`ADR-RB-05` #3
    /// Case D, `RB-08d`, `RB-08u`, `RB-08k`, `RB-08v`).
    CaseDRecoverValidSuccessor {
        meta: LaneTailMeta,
        old_tail: Option<NestedOldTailPlan>,
        succ_seg: u32,
        succ_valid_end: u64,
        succ_crc32c: u32,
        succ_frames: usize,
        succ_has_damaged_tail: bool,
        stale_tmp_names: Vec<T>,
        reclaimed_orphans: Vec<u32>,
    },
    /// Case D (empty or half-frame-only orphan successor `S + 1`, optionally
    /// accompanied by a nested Case C old-segment tail `old_tail` on `S`):
    /// `max_live_seg == active_seg + 1`, old segment `S` `[0, committed_len)`
    /// prefix CRC32C verified, and `events-<S+1>.log` has `0` complete valid
    /// frames (`orphan_non_empty == false` for 0-byte empty orphan,
    /// `orphan_non_empty == true` for half-frame/damaged-only orphan).
    /// Phase 2 recovers any nested old tail via `case_c_step_c1`, quarantines
    /// non-empty orphan via `RB-08t::quarantine_and_unlink_orphan`, unlinks the
    /// orphan, `fsync_dir(lane_dir)`, and syncs dual `LTM2` replicas to the
    /// intact `active_seg` without sealing `S` (`ADR-RB-05` #3 Case D, `RB-08f`,
    /// `RB-08k`).
    CaseDCleanOrphanSuccessor {
        meta: LaneTailMeta,
        old_tail: Option<NestedOldTailPlan>,
        orphan_seg: u32,
        orphan_non_empty: bool,
        stale_tmp_names: Vec<T>,
        reclaimed_orphans: Vec<u32>,
    },
}

// ============================================================================
// Durability Observer & Crash Fault-Injection Seam (Zero-Cost in Non-Test)
// ============================================================================

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum SegmentOpEvent {
    AcquireAppendLock,
    CreateSegment {
        lane: &'static str,
        seg_id: u32,
    },
    FsyncLaneDirAfterCreateSegment {
        lane: &'static str,
        seg_id: u32,
    },
    SegmentWriteAll {
        lane: &'static str,
        seg_id: u32,
        offset: u64,
        len: usize,
    },
    SegmentSyncData {
        lane: &'static str,
        seg_id: u32,
    },
    MetaTmpWriteAndSync {
        lane: &'static str,
        tmp_name: &'static str,
        target_name: &'static str,
    },
    MetaRename {
        lane: &'static str,
        tmp_name: &'static str,
        target_name: &'static str,
    },
    FsyncLaneDirAfterMeta {
        lane: &'static str,
        target_name: &'static str,
    },
    CaseCRecoverFrames {
        lane: &'static str,
        seg_id: u32,
        from_offset: u64,
        to_offset: u64,
        frame_count: usize,
    },
    CaseDRecoverCleanSuccessor {
        lane: &'static str,
        old_seg_id: u32,
        succ_seg_id: u32,
        succ_len: u64,
        frame_count: usize,
    },
    QuarantineTmpSyncDataStart {
        id: String,
    },
    QuarantineTmpSyncDataComplete {
        id: String,
    },
    QuarantineRenameStart {
        id: String,
    },
    QuarantineRenameComplete {
        id: String,
    },
    QuarantineFsyncDirStart {
        id: String,
    },
    QuarantineFsyncDirComplete {
        id: String,
    },
    SegmentTruncateStart {
        lane: &'static str,
        seg_id: u32,
        to_len: u64,
    },
    SegmentTruncateComplete {
        lane: &'static str,
        seg_id: u32,
        to_len: u64,
    },
    OrphanUnlinkStart {
        lane: &'static str,
        seg_id: u32,
    },
    OrphanUnlinkComplete {
        lane: &'static str,
        seg_id: u32,
    },
    FsyncLaneDirAfterOrphanUnlink {
        lane: &'static str,
        seg_id: u32,
    },
    FsyncLaneDirOnRecovery {
        lane: &'static str,
    },
    ReclaimSegmentUnlink {
        lane: &'static str,
        seg_id: u32,
    },
    FsyncLaneDirAfterReclaim {
        lane: &'static str,
        seg_id: u32,
    },
    CapacityChecked {
        priority_bytes: u64,
        normal_bytes: u64,
        quarantine_bytes: u64,
        incoming_bytes: usize,
        max_total_bytes: u64,
    },
    ReleaseAppendLock,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentCrashPoint {
    None,
    /// Simulate process crash immediately after `seg.sync_data()` and before
    /// any `tail.meta` or `tail.meta.bak` update (Case C fault injection).
    AfterSegmentSyncData,
    /// Simulate process crash after `tail.meta.tmp` `sync_data()` and before
    /// `rename("tail.meta.tmp", "tail.meta")`, leaving `tail.meta.tmp` on disk.
    BeforePrimaryMetaRename,
    /// Simulate process crash immediately after `tail.meta` atomic replace and
    /// `fsync_dir(lane_dir)`, before updating `tail.meta.bak`.
    AfterPrimaryMetaCommit,
    /// Simulate process crash after `tail.meta.bak.tmp` `sync_data()` and
    /// before `rename("tail.meta.bak.tmp", "tail.meta.bak")`, leaving
    /// `tail.meta.bak.tmp` on disk.
    BeforeBackupMetaRename,
    /// Simulate process crash after `quarantine/<id>.tmp` `sync_data()` and
    /// strictly before `rename(.tmp -> .bin)`, leaving `<id>.tmp` on disk and
    /// leaving the source segment completely unmodified (`RB-08t` AC6–AC7).
    BeforeQuarantineRename,
    /// Simulate process crash immediately after `fsync_dir(quarantine_dir)`
    /// and strictly before destructive `truncate` or `unlink`, leaving
    /// `quarantine/<id>.bin` durably persisted (`RB-08t` AC8).
    AfterQuarantineDirFsyncBeforeDestructive,
    /// Simulate transient `fsync_dir(lane_dir)` failure immediately after
    /// `unlink(orphan_seg)` succeeded, verifying that retry persists the lane
    /// directory before returning `Ok` (`RB-08f` AC2/AC5 retry durability).
    FailLaneDirFsyncAfterOrphanUnlink,
    /// Simulate process crash immediately after Case C Step C1 `truncate(S)` +
    /// `seg.sync_data()` and strictly before Step C1 dual-replica `LTM2` commit
    /// (`RB-08i` Boundary 2).
    AfterCaseCStepC1TruncateSyncBeforeMeta,
    /// Simulate process crash immediately after Case C Step C1 dual-replica
    /// `LTM2` commit and strictly before Step C2 `seal_recovered_segment_to_next`
    /// creates `events-<S+1>.log` (`RB-08m` / `RB-08n`).
    AfterCaseCStepC1MetaBeforeStepC2,
    /// Simulate process crash immediately after Step C2 creates empty
    /// `events-<S+1>.log` + `fsync_dir(lane_dir)` and strictly before Step C2
    /// dual-replica `LTM2` commit (`RB-08i` Boundary 3).
    AfterStepC2CreateSegmentBeforeMeta,
    /// Simulate process crash immediately after `reclaim_sealed_segment`
    /// commits dual-replica `LTM2` (advancing `MIN_LIVE_SEG` and removing
    /// `seg_id` from `sealed[]`) and strictly before `unlink(events-<seg_id>)`
    /// (`ADR-RB-05` #4 ③, `RB-08j`).
    AfterReclaimMetaBeforeUnlink,
}

#[cfg(test)]
type BeforeCreateMissingLockHook = Box<dyn FnMut()>;

#[cfg(test)]
struct SegmentObserver {
    events: Vec<SegmentOpEvent>,
    crash_point: SegmentCrashPoint,
    before_create_missing_lock: Option<BeforeCreateMissingLockHook>,
    simulate_foreign_reclaim_owner: bool,
}

#[cfg(test)]
impl SegmentObserver {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            crash_point: SegmentCrashPoint::None,
            before_create_missing_lock: None,
            simulate_foreign_reclaim_owner: false,
        }
    }

    fn with_crash(crash_point: SegmentCrashPoint) -> Self {
        Self {
            events: Vec::new(),
            crash_point,
            before_create_missing_lock: None,
            simulate_foreign_reclaim_owner: false,
        }
    }

    fn with_before_create_missing_lock(hook: impl FnMut() + 'static) -> Self {
        Self {
            events: Vec::new(),
            crash_point: SegmentCrashPoint::None,
            before_create_missing_lock: Some(Box::new(hook)),
            simulate_foreign_reclaim_owner: false,
        }
    }

    fn with_foreign_reclaim_owner() -> Self {
        Self {
            events: Vec::new(),
            crash_point: SegmentCrashPoint::None,
            before_create_missing_lock: None,
            simulate_foreign_reclaim_owner: true,
        }
    }

    fn simulate_foreign_reclaim_owner(&self) -> bool {
        self.simulate_foreign_reclaim_owner
    }

    fn on_before_create_missing_lock(&mut self) {
        if let Some(hook) = self.before_create_missing_lock.as_mut() {
            hook();
        }
    }

    fn record_acquire_lock(&mut self) {
        self.events.push(SegmentOpEvent::AcquireAppendLock);
    }

    fn record_create_segment(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::CreateSegment { lane, seg_id });
    }

    fn record_fsync_lane_after_create_segment(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::FsyncLaneDirAfterCreateSegment { lane, seg_id });
    }

    fn record_segment_write_all(
        &mut self,
        lane: &'static str,
        seg_id: u32,
        offset: u64,
        len: usize,
    ) {
        self.events.push(SegmentOpEvent::SegmentWriteAll {
            lane,
            seg_id,
            offset,
            len,
        });
    }

    fn record_segment_sync_data(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::SegmentSyncData { lane, seg_id });
    }

    fn record_meta_tmp_synced(
        &mut self,
        lane: &'static str,
        tmp_name: &'static str,
        target_name: &'static str,
    ) {
        self.events.push(SegmentOpEvent::MetaTmpWriteAndSync {
            lane,
            tmp_name,
            target_name,
        });
    }

    fn record_meta_renamed(
        &mut self,
        lane: &'static str,
        tmp_name: &'static str,
        target_name: &'static str,
    ) {
        self.events.push(SegmentOpEvent::MetaRename {
            lane,
            tmp_name,
            target_name,
        });
    }

    fn record_fsync_lane_after_meta(&mut self, lane: &'static str, target_name: &'static str) {
        self.events
            .push(SegmentOpEvent::FsyncLaneDirAfterMeta { lane, target_name });
    }

    fn record_case_c_recover(
        &mut self,
        lane: &'static str,
        seg_id: u32,
        from_offset: u64,
        to_offset: u64,
        frame_count: usize,
    ) {
        self.events.push(SegmentOpEvent::CaseCRecoverFrames {
            lane,
            seg_id,
            from_offset,
            to_offset,
            frame_count,
        });
    }

    fn record_case_d_recover_successor(
        &mut self,
        lane: &'static str,
        old_seg_id: u32,
        succ_seg_id: u32,
        succ_len: u64,
        frame_count: usize,
    ) {
        self.events
            .push(SegmentOpEvent::CaseDRecoverCleanSuccessor {
                lane,
                old_seg_id,
                succ_seg_id,
                succ_len,
                frame_count,
            });
    }

    fn record_quarantine_tmp_sync_start(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineTmpSyncDataStart { id: id.to_string() });
    }

    fn record_quarantine_tmp_sync_complete(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineTmpSyncDataComplete { id: id.to_string() });
    }

    fn record_quarantine_rename_start(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineRenameStart { id: id.to_string() });
    }

    fn record_quarantine_rename_complete(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineRenameComplete { id: id.to_string() });
    }

    fn record_quarantine_fsync_dir_start(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineFsyncDirStart { id: id.to_string() });
    }

    fn record_quarantine_fsync_dir_complete(&mut self, id: &str) {
        self.events
            .push(SegmentOpEvent::QuarantineFsyncDirComplete { id: id.to_string() });
    }

    fn record_segment_truncate_start(&mut self, lane: &'static str, seg_id: u32, to_len: u64) {
        self.events.push(SegmentOpEvent::SegmentTruncateStart {
            lane,
            seg_id,
            to_len,
        });
    }

    fn record_segment_truncate_complete(&mut self, lane: &'static str, seg_id: u32, to_len: u64) {
        self.events.push(SegmentOpEvent::SegmentTruncateComplete {
            lane,
            seg_id,
            to_len,
        });
    }

    fn record_orphan_unlink_start(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::OrphanUnlinkStart { lane, seg_id });
    }

    fn record_orphan_unlink_complete(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::OrphanUnlinkComplete { lane, seg_id });
    }

    fn record_fsync_lane_after_orphan_unlink(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink { lane, seg_id });
    }

    fn record_fsync_lane_on_recovery(&mut self, lane: &'static str) {
        self.events
            .push(SegmentOpEvent::FsyncLaneDirOnRecovery { lane });
    }

    fn record_reclaim_segment_unlink(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::ReclaimSegmentUnlink { lane, seg_id });
    }

    fn record_fsync_lane_after_reclaim(&mut self, lane: &'static str, seg_id: u32) {
        self.events
            .push(SegmentOpEvent::FsyncLaneDirAfterReclaim { lane, seg_id });
    }

    fn record_capacity_checked(
        &mut self,
        occupancy: CaptureQueueOccupancy,
        incoming_bytes: usize,
        max_total_bytes: u64,
    ) {
        self.events.push(SegmentOpEvent::CapacityChecked {
            priority_bytes: occupancy.priority_bytes,
            normal_bytes: occupancy.normal_bytes,
            quarantine_bytes: occupancy.quarantine_bytes,
            incoming_bytes,
            max_total_bytes,
        });
    }

    fn record_release_lock(&mut self) {
        self.events.push(SegmentOpEvent::ReleaseAppendLock);
    }

    fn maybe_crash(&self, point: SegmentCrashPoint) -> StorageResult<()> {
        if self.crash_point == point && point != SegmentCrashPoint::None {
            return Err(StorageBackendError::Io {
                context: "simulated capture queue producer crash",
                kind: io::ErrorKind::Interrupted,
                raw_os_error: None,
            });
        }
        Ok(())
    }
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentCrashPoint {
    AfterSegmentSyncData,
    BeforePrimaryMetaRename,
    AfterPrimaryMetaCommit,
    BeforeBackupMetaRename,
    BeforeQuarantineRename,
    AfterQuarantineDirFsyncBeforeDestructive,
    FailLaneDirFsyncAfterOrphanUnlink,
    AfterCaseCStepC1TruncateSyncBeforeMeta,
    AfterCaseCStepC1MetaBeforeStepC2,
    AfterStepC2CreateSegmentBeforeMeta,
    AfterReclaimMetaBeforeUnlink,
}

#[cfg(not(test))]
struct SegmentObserver;

#[cfg(not(test))]
impl SegmentObserver {
    const fn new() -> Self {
        Self
    }

    #[inline]
    fn record_acquire_lock(&mut self) {}

    #[inline]
    fn record_create_segment(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_fsync_lane_after_create_segment(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_segment_write_all(
        &mut self,
        _lane: &'static str,
        _seg_id: u32,
        _offset: u64,
        _len: usize,
    ) {
    }

    #[inline]
    fn record_segment_sync_data(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_meta_tmp_synced(
        &mut self,
        _lane: &'static str,
        _tmp_name: &'static str,
        _target_name: &'static str,
    ) {
    }

    #[inline]
    fn record_meta_renamed(
        &mut self,
        _lane: &'static str,
        _tmp_name: &'static str,
        _target_name: &'static str,
    ) {
    }

    #[inline]
    fn record_fsync_lane_after_meta(&mut self, _lane: &'static str, _target_name: &'static str) {}

    #[inline]
    fn record_case_c_recover(
        &mut self,
        _lane: &'static str,
        _seg_id: u32,
        _from_offset: u64,
        _to_offset: u64,
        _frame_count: usize,
    ) {
    }

    #[inline]
    fn record_case_d_recover_successor(
        &mut self,
        _lane: &'static str,
        _old_seg_id: u32,
        _succ_seg_id: u32,
        _succ_len: u64,
        _frame_count: usize,
    ) {
    }

    #[inline]
    fn record_quarantine_tmp_sync_start(&mut self, _id: &str) {}

    #[inline]
    fn record_quarantine_tmp_sync_complete(&mut self, _id: &str) {}

    #[inline]
    fn record_quarantine_rename_start(&mut self, _id: &str) {}

    #[inline]
    fn record_quarantine_rename_complete(&mut self, _id: &str) {}

    #[inline]
    fn record_quarantine_fsync_dir_start(&mut self, _id: &str) {}

    #[inline]
    fn record_quarantine_fsync_dir_complete(&mut self, _id: &str) {}

    #[inline]
    fn record_segment_truncate_start(&mut self, _lane: &'static str, _seg_id: u32, _to_len: u64) {}

    #[inline]
    fn record_segment_truncate_complete(
        &mut self,
        _lane: &'static str,
        _seg_id: u32,
        _to_len: u64,
    ) {
    }

    #[inline]
    fn record_orphan_unlink_start(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_orphan_unlink_complete(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_fsync_lane_after_orphan_unlink(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_fsync_lane_on_recovery(&mut self, _lane: &'static str) {}

    #[inline]
    fn record_reclaim_segment_unlink(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_fsync_lane_after_reclaim(&mut self, _lane: &'static str, _seg_id: u32) {}

    #[inline]
    fn record_capacity_checked(
        &mut self,
        _occupancy: CaptureQueueOccupancy,
        _incoming_bytes: usize,
        _max_total_bytes: u64,
    ) {
    }

    #[inline]
    fn record_release_lock(&mut self) {}

    #[inline]
    fn maybe_crash(&self, _point: SegmentCrashPoint) -> StorageResult<()> {
        Ok(())
    }

    #[inline]
    fn on_before_create_missing_lock(&mut self) {}

    #[inline]
    fn simulate_foreign_reclaim_owner(&self) -> bool {
        false
    }
}

// ============================================================================
// Shared Streaming Prefix CRC32C & Case C Tail Scan Helpers
// ============================================================================

fn read_exact_chunk(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// Stream-verify hardware Castagnoli `crc32c(file[0..committed_len]) == expected_crc`
/// using a fixed 64 KiB stack buffer (zero heap allocation).
///
/// Leaves `file` positioned at exact byte offset `committed_len` on success.
fn verify_committed_prefix_crc(
    file: &mut File,
    committed_len: u64,
    expected_crc: u32,
) -> StorageResult<()> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| StorageBackendError::io("seek active segment for prefix CRC32C", e))?;

    let mut remaining = committed_len;
    let mut running_crc = 0u32;
    let mut buf = [0u8; PREFIX_CRC_CHUNK_BYTES];

    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let read_n = read_exact_chunk(file, &mut buf[..want])
            .map_err(|e| StorageBackendError::io("read active segment committed prefix", e))?;
        if read_n < want {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail:
                    "active segment ended before committed_len during prefix CRC32C verification"
                        .to_string(),
            });
        }
        running_crc = crc32c_append(running_crc, &buf[..read_n]);
        remaining -= read_n as u64;
    }

    if running_crc != expected_crc {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "active segment committed prefix [0, committed_len) CRC32C mismatch"
                .to_string(),
        });
    }
    Ok(())
}

/// Stream-extend `initial_crc` over exact byte range `[start_offset, start_offset + byte_len)`.
fn extend_crc32c_over_file_range(
    file: &mut File,
    start_offset: u64,
    byte_len: u64,
    initial_crc: u32,
) -> StorageResult<u32> {
    file.seek(SeekFrom::Start(start_offset))
        .map_err(|e| StorageBackendError::io("seek active segment for tail CRC32C extension", e))?;

    let mut remaining = byte_len;
    let mut running_crc = initial_crc;
    let mut buf = [0u8; PREFIX_CRC_CHUNK_BYTES];

    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let read_n = read_exact_chunk(file, &mut buf[..want])
            .map_err(|e| StorageBackendError::io("read active segment tail for CRC32C", e))?;
        if read_n < want {
            return Err(StorageBackendError::io(
                "short read while extending CRC32C over verified tail frames",
                io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof"),
            ));
        }
        running_crc = crc32c_append(running_crc, &buf[..read_n]);
        remaining -= read_n as u64;
    }

    Ok(running_crc)
}

struct CaseCTailScan {
    valid_end: u64,
    recovered_crc32c: u32,
    frame_count: usize,
    has_damaged_tail: bool,
}

/// Read-only forward-scan `[committed_len, actual_len)` on `file` (after
/// [`verify_committed_prefix_crc`]) to recover all complete dual-CRC32C frames
/// written after a crash and detect any trailing damaged/truncated half-frame
/// bytes `[valid_end, actual_len)` (`ADR-RB-05` #3 Case C, `RB-08`, `RB-08m`).
fn scan_case_c_uncommitted_tail(
    file: &mut File,
    committed_len: u64,
    actual_len: u64,
    prefix_crc32c: u32,
) -> StorageResult<CaseCTailScan> {
    debug_assert!(actual_len > committed_len);
    file.seek(SeekFrom::Start(committed_len))
        .map_err(|e| StorageBackendError::io("seek active segment for Case C tail scan", e))?;
    let mut decoder = FrameStreamDecoder::with_known_len(&mut *file, committed_len, actual_len);
    let mut valid_end = committed_len;
    let mut frame_count = 0usize;
    let mut damaged_tail = false;

    loop {
        match decoder.next_frame() {
            Ok(FrameStreamItem::Frame {
                offset, total_len, ..
            }) => {
                let len_u64 =
                    u64::try_from(total_len).map_err(|_| StorageBackendError::BackendInternal {
                        detail: "decoded frame length exceeds u64".to_string(),
                    })?;
                valid_end = offset.checked_add(len_u64).ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "case C recovered offset overflow".to_string(),
                    }
                })?;
                frame_count += 1;
            }
            Ok(FrameStreamItem::CleanEof { offset }) => {
                valid_end = offset;
                break;
            }
            Err(
                FrameDecodeError::TruncatedTail { .. }
                | FrameDecodeError::CorruptHeader { .. }
                | FrameDecodeError::CorruptPayload { .. },
            ) => {
                damaged_tail = true;
                break;
            }
            Err(FrameDecodeError::Io { kind }) => {
                return Err(StorageBackendError::Io {
                    context: "scan uncommitted capture queue tail frames",
                    kind,
                    raw_os_error: None,
                });
            }
        }
    }

    let has_damaged_tail = damaged_tail || valid_end < actual_len;
    let delta = valid_end.checked_sub(committed_len).ok_or_else(|| {
        StorageBackendError::BackendInternal {
            detail: "case C valid_end regressed below committed_len".to_string(),
        }
    })?;
    let recovered_crc32c =
        extend_crc32c_over_file_range(file, committed_len, delta, prefix_crc32c)?;

    Ok(CaseCTailScan {
        valid_end,
        recovered_crc32c,
        frame_count,
        has_damaged_tail,
    })
}

/// Read-only scan result over a Case D successor segment `events-<S+1>.log`
/// starting from byte offset `0` (`ADR-RB-05` #3 Case D, `RB-08d`).
struct SuccessorSegmentScan {
    valid_end: u64,
    valid_crc32c: u32,
    frame_count: usize,
    has_damaged_tail: bool,
}

/// Validate that `live_segs` contains strictly a single contiguous successor
/// segment `active_seg + 1` beyond `active_seg` (zero gaps, zero multi-segment
/// leaps, zero `u32` overflow).
fn validate_single_case_d_successor_id(active_seg: u32, live_segs: &[u32]) -> StorageResult<u32> {
    let succ_seg =
        active_seg
            .checked_add(1)
            .ok_or_else(|| StorageBackendError::CorruptCommittedFrame {
                detail: "Case D: active_seg overflow computing successor segment id".to_string(),
            })?;
    let mut successors = live_segs.iter().copied().filter(|&id| id > active_seg);
    let first = successors.next();
    let second = successors.next();
    if first != Some(succ_seg) || second.is_some() {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail:
                "Case D: unexpected non-contiguous or multiple successor segments beyond active_seg"
                    .to_string(),
        });
    }
    Ok(succ_seg)
}

/// Strictly read-only forward scan of a Case D successor segment `events-<S+1>.log`
/// from offset `0` to `actual_len` (`ADR-RB-05` #3 Case D, `RB-08d`).
fn scan_successor_segment_readonly(
    file: &mut File,
    actual_len: u64,
) -> StorageResult<SuccessorSegmentScan> {
    if actual_len == 0 {
        return Ok(SuccessorSegmentScan {
            valid_end: 0,
            valid_crc32c: 0,
            frame_count: 0,
            has_damaged_tail: false,
        });
    }

    file.seek(SeekFrom::Start(0))
        .map_err(|e| StorageBackendError::io("seek successor segment for Case D scan", e))?;
    let mut decoder = FrameStreamDecoder::with_known_len(&mut *file, 0, actual_len);
    let mut valid_end = 0u64;
    let mut frame_count = 0usize;
    let mut damaged_tail = false;

    loop {
        match decoder.next_frame() {
            Ok(FrameStreamItem::Frame {
                offset, total_len, ..
            }) => {
                let len_u64 =
                    u64::try_from(total_len).map_err(|_| StorageBackendError::BackendInternal {
                        detail: "decoded successor frame length exceeds u64".to_string(),
                    })?;
                valid_end = offset.checked_add(len_u64).ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "Case D successor frame offset overflow".to_string(),
                    }
                })?;
                frame_count += 1;
            }
            Ok(FrameStreamItem::CleanEof { offset }) => {
                valid_end = offset;
                break;
            }
            Err(
                FrameDecodeError::TruncatedTail { .. }
                | FrameDecodeError::CorruptHeader { .. }
                | FrameDecodeError::CorruptPayload { .. },
            ) => {
                damaged_tail = true;
                break;
            }
            Err(FrameDecodeError::Io { kind }) => {
                return Err(StorageBackendError::Io {
                    context: "scan Case D successor segment frames",
                    kind,
                    raw_os_error: None,
                });
            }
        }
    }

    let valid_crc32c = if valid_end > 0 {
        extend_crc32c_over_file_range(file, 0, valid_end, 0)?
    } else {
        0
    };

    Ok(SuccessorSegmentScan {
        valid_end,
        valid_crc32c,
        frame_count,
        has_damaged_tail: damaged_tail || valid_end < actual_len,
    })
}

/// Phase 1 read-only classification result for a single contiguous Case D
/// successor `events-<S+1>.log` (`ADR-RB-05` #3 Case D, `RB-08d`/`RB-08u`/`RB-08f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseDPhase1Kind {
    ValidSuccessor { has_damaged_tail: bool },
    OrphanSuccessor { non_empty: bool },
}

/// Early Phase 1 read-only guard for `RB-08r`: whenever a Case D successor
/// segment `S + 1` decodes `>= 1` valid frame while `LTM2.sealed[]` is already
/// at `MAX_SEALED_SEGMENTS` (`256`), immediately fail closed with
/// `CorruptCommittedFrame` (`LBR-IO-001`) before scanning any nested old tail
/// or performing any Phase 2 mutation.
fn ensure_case_d_sealed_capacity(
    meta: &LaneTailMeta,
    succ_scan: &SuccessorSegmentScan,
) -> StorageResult<()> {
    if succ_scan.frame_count > 0 && meta.sealed.len() >= MAX_SEALED_SEGMENTS {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail:
                "Case D: valid successor segment exists while LTM2 sealed table is already at MAX_SEALED_SEGMENTS (256)"
                    .to_string(),
        });
    }
    Ok(())
}

/// Apply Phase 1 read-only classification rules for Case D (`RB-08d` clean
/// valid successor, `RB-08u` valid successor with trailing uncommitted
/// half-frame, `RB-08f` empty or half-frame-only orphan successor, `RB-08k`
/// nested Case C→D clean synced old-segment tail, `RB-08v` nested Case C→D
/// damaged old-segment tail, and `RB-08r` full 256-entry table fail-closed guard).
///
/// Validates `MAX_SEALED_SEGMENTS` illegal full-table state strictly first
/// (before any mutation) and forbids sealing a 0-byte old segment.
fn classify_case_d_readonly(
    meta: &LaneTailMeta,
    old_tail: Option<&NestedOldTailPlan>,
    succ_actual_len: u64,
    succ_scan: &SuccessorSegmentScan,
) -> StorageResult<CaseDPhase1Kind> {
    ensure_case_d_sealed_capacity(meta, succ_scan)?;
    if succ_scan.frame_count == 0 {
        return Ok(CaseDPhase1Kind::OrphanSuccessor {
            non_empty: succ_actual_len > 0,
        });
    }
    let effective_old_len = old_tail.map_or(meta.committed_len, |ot| ot.valid_end);
    if effective_old_len == 0 {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail:
                "Case D: valid successor segment exists while old active segment is empty (0 bytes)"
                    .to_string(),
        });
    }
    Ok(CaseDPhase1Kind::ValidSuccessor {
        has_damaged_tail: succ_scan.has_damaged_tail,
    })
}

/// Build and validate the converged `LTM2` metadata that seals clean old segment
/// `(old_meta.active_seg, old_meta.committed_len, old_meta.prefix_crc32c)` into
/// `sealed[]` and advances `(ACTIVE_SEG, COMMITTED_LEN, PREFIX_CRC32C)` to the
/// recovered clean successor `succ_seg` (`ADR-RB-05` #3 Case D, `RB-08d` AC4–AC7).
fn build_case_d_sealed_successor_meta(
    old_meta: &LaneTailMeta,
    succ_seg: u32,
    succ_valid_end: u64,
    succ_crc32c: u32,
) -> StorageResult<LaneTailMeta> {
    if old_meta.sealed.len() >= MAX_SEALED_SEGMENTS {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail:
                "Case D: cannot seal old active segment when LTM2 sealed table is already at MAX_SEALED_SEGMENTS (256)"
                    .to_string(),
        });
    }
    let mut sealed = Vec::with_capacity(old_meta.sealed.len() + 1);
    sealed.extend_from_slice(&old_meta.sealed);
    sealed.push(SealedSegmentMeta {
        seg_id: old_meta.active_seg,
        sealed_len: old_meta.committed_len,
        sealed_crc32c: old_meta.prefix_crc32c,
    });
    let next_meta = LaneTailMeta {
        min_live_seg: old_meta.min_live_seg,
        active_seg: succ_seg,
        committed_len: succ_valid_end,
        prefix_crc32c: succ_crc32c,
        sealed,
    };
    next_meta
        .validate()
        .map_err(Ltm2CodecError::into_storage_error)?;
    Ok(next_meta)
}

/// Build and validate the `ADR-RB-05` #3 Case C **Step C2** `LTM2` metadata
/// (`RB-08n` AC2–AC5): given the committed Step C1 `LTM2` metadata for segment
/// `S` (`active_seg == S`, `committed_len == recovered_valid_end`,
/// `prefix_crc32c == recovered_crc32c`), seal `(S, recovered_valid_end,
/// recovered_crc32c)` into `LTM2.sealed[]`, advance `ACTIVE_SEG = S + 1`, and
/// reset `COMMITTED_LEN = 0` and `PREFIX_CRC32C = 0` for the newly created
/// empty segment `S + 1`.
fn build_case_c_step_c2_meta(step_c1_meta: &LaneTailMeta) -> StorageResult<LaneTailMeta> {
    if step_c1_meta.committed_len == 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "Case C Step C2 must not seal a 0-byte active segment".to_string(),
        });
    }
    let next_seg = step_c1_meta.active_seg.checked_add(1).ok_or_else(|| {
        StorageBackendError::BackendInternal {
            detail: "capture queue segment id overflow during Case C Step C2".to_string(),
        }
    })?;
    let mut sealed = Vec::with_capacity(step_c1_meta.sealed.len() + 1);
    sealed.extend_from_slice(&step_c1_meta.sealed);
    sealed.push(SealedSegmentMeta {
        seg_id: step_c1_meta.active_seg,
        sealed_len: step_c1_meta.committed_len,
        sealed_crc32c: step_c1_meta.prefix_crc32c,
    });
    let next_meta = LaneTailMeta {
        min_live_seg: step_c1_meta.min_live_seg,
        active_seg: next_seg,
        committed_len: 0,
        prefix_crc32c: 0,
        sealed,
    };
    next_meta
        .validate()
        .map_err(Ltm2CodecError::into_storage_error)?;
    Ok(next_meta)
}

/// Validate a caller-supplied pre-encoded single frame buffer strictly outside
/// `append.lock`.
fn validate_single_encoded_frame(encoded_frame: &[u8]) -> StorageResult<()> {
    let (_, consumed) =
        decode_frame(encoded_frame).map_err(FrameDecodeError::into_storage_error)?;
    if consumed != encoded_frame.len() {
        return Err(StorageBackendError::BackendInternal {
            detail: "encoded frame buffer contains trailing bytes beyond single LCE1 frame"
                .to_string(),
        });
    }
    Ok(())
}

// ============================================================================
// Public Entry Points
// ============================================================================

/// Encode a single capture frame outside `append.lock` and durably append it to
/// `<storage_dir>/private/capture-queue/<lane>/` (`ADR-RB-05` #3, `RB-08`, `RB-08b`).
pub fn append_frame(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    append_frame_with_options(
        storage_dir,
        lane,
        flags,
        dedup_key,
        postcard_envelope,
        QueueAppendOptions::default(),
    )
}

/// Encode a single capture frame outside `append.lock` and durably append it to
/// `<storage_dir>/private/capture-queue/<lane>/` with explicit bounded
/// capacity and lock-wait options (`ADR-RB-05` #3, `RB-08b`).
pub fn append_frame_with_options(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
    options: QueueAppendOptions,
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_frame(flags, dedup_key, postcard_envelope)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir_with_options(
        storage_dir,
        lane,
        &frame_buf,
        options,
        &mut observer,
    )
}

/// Encode a single capture frame outside `append.lock` and durably append it to
/// `<queue_root>/<lane>/` (`ADR-RB-05` #3, `RB-08`, `RB-08b`).
pub fn append_frame_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    append_frame_at_root_with_options(
        queue_root,
        lane,
        flags,
        dedup_key,
        postcard_envelope,
        QueueAppendOptions::default(),
    )
}

/// Encode a single capture frame outside `append.lock` and durably append it to
/// `<queue_root>/<lane>/` with explicit bounded capacity and lock-wait options
/// (`ADR-RB-05` #3, `RB-08b`).
pub fn append_frame_at_root_with_options(
    queue_root: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
    options: QueueAppendOptions,
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_frame(flags, dedup_key, postcard_envelope)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_at_root_with_options(
        queue_root,
        lane,
        &frame_buf,
        options,
        &mut observer,
    )
}

/// Serialize `value` with versioned `postcard` framing outside `append.lock`
/// and durably append it to `<storage_dir>/private/capture-queue/<lane>/`
/// (`ADR-RB-05` #3, `RB-08`, `RB-08b`).
pub fn append_postcard_frame<T: Serialize>(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    schema_version: u16,
    value: &T,
) -> StorageResult<SegmentAppendOutcome> {
    append_postcard_frame_with_options(
        storage_dir,
        lane,
        flags,
        dedup_key,
        schema_version,
        value,
        QueueAppendOptions::default(),
    )
}

/// Serialize `value` with versioned `postcard` framing outside `append.lock`
/// and durably append it to `<storage_dir>/private/capture-queue/<lane>/`
/// with explicit bounded capacity and lock-wait options (`ADR-RB-05` #3, `RB-08b`).
pub fn append_postcard_frame_with_options<T: Serialize>(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    schema_version: u16,
    value: &T,
    options: QueueAppendOptions,
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_postcard_frame(flags, dedup_key, schema_version, value)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir_with_options(
        storage_dir,
        lane,
        &frame_buf,
        options,
        &mut observer,
    )
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<storage_dir>/private/capture-queue/<lane>/` (`ADR-RB-05` #3, `RB-08`, `RB-08b`).
pub fn append_encoded_frame(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    append_encoded_frame_with_options(
        storage_dir,
        lane,
        encoded_frame,
        QueueAppendOptions::default(),
    )
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<storage_dir>/private/capture-queue/<lane>/` with explicit bounded
/// capacity and lock-wait options (`ADR-RB-05` #3, `RB-08b`).
pub fn append_encoded_frame_with_options(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
    options: QueueAppendOptions,
) -> StorageResult<SegmentAppendOutcome> {
    validate_single_encoded_frame(encoded_frame)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir_with_options(
        storage_dir,
        lane,
        encoded_frame,
        options,
        &mut observer,
    )
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<queue_root>/<lane>/` (`ADR-RB-05` #3, `RB-08`, `RB-08b`).
pub fn append_encoded_frame_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    append_encoded_frame_at_root_with_options(
        queue_root,
        lane,
        encoded_frame,
        QueueAppendOptions::default(),
    )
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<queue_root>/<lane>/` with explicit bounded capacity and lock-wait
/// options (`ADR-RB-05` #3, `RB-08b`).
pub fn append_encoded_frame_at_root_with_options(
    queue_root: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
    options: QueueAppendOptions,
) -> StorageResult<SegmentAppendOutcome> {
    validate_single_encoded_frame(encoded_frame)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_at_root_with_options(
        queue_root,
        lane,
        encoded_frame,
        options,
        &mut observer,
    )
}

/// Measure combined cross-lane physical byte occupancy (`unconsumed_priority +
/// unconsumed_normal + quarantine`) under `<storage_dir>/private/capture-queue`
/// (`ADR-RB-05` #3, `RB-08b`).
///
/// Acquires shared `append.lock` when present (bounded by
/// [`DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT`]) for a consistent cross-lane snapshot
/// and performs zero disk mutations.
pub fn measure_capture_queue_occupancy(storage_dir: &Path) -> StorageResult<CaptureQueueOccupancy> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::measure_occupancy_in_storage_dir_unix(storage_dir, owners)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::measure_occupancy_in_storage_dir_non_unix(storage_dir)
    }
}

/// Measure combined cross-lane physical byte occupancy (`unconsumed_priority +
/// unconsumed_normal + quarantine`) directly under `queue_root` (`ADR-RB-05`
/// #3, `RB-08b`).
///
/// Acquires shared `append.lock` when present (bounded by
/// [`DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT`]) for a consistent cross-lane snapshot
/// and performs zero disk mutations.
pub fn measure_capture_queue_occupancy_at_root(
    queue_root: &Path,
) -> StorageResult<CaptureQueueOccupancy> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::measure_occupancy_at_root_unix(queue_root, owners)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::measure_occupancy_at_root_non_unix(queue_root)
    }
}

/// Outcome of crash-safely reclaiming the oldest consumed sealed segment
/// (`seg_id == min_live_seg && seg_id < active_seg && seg_id < highwater_seg`)
/// under the `writer.lock -> capture-queue/append.lock` protocol
/// (`ADR-RB-05` #4 ③, `RB-08j`, `RB-09d`, `RB-09k`, `RB-09l`).
#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SegmentReclaimOutcome {
    pub(super) lane: CaptureQueueLane,
    pub(super) reclaimed_seg_id: u32,
    pub(super) meta: LaneTailMeta,
    pub(super) file_removed: bool,
}

/// Execute read-only Phase 1 classification and Phase 2 convergence / Case C
/// same-segment recovery on `<storage_dir>/private/capture-queue/<lane>/` under
/// shared `append.lock` without appending a new frame (`RB-08`).
pub fn recover_lane(
    storage_dir: &Path,
    lane: CaptureQueueLane,
) -> StorageResult<LaneRecoveryReport> {
    let mut observer = SegmentObserver::new();
    recover_lane_in_storage_dir(storage_dir, lane, &mut observer)
}

/// Execute read-only Phase 1 classification and Phase 2 convergence / Case C
/// same-segment recovery on `<queue_root>/<lane>/` under shared `append.lock`
/// without appending a new frame (`RB-08`).
pub fn recover_lane_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
) -> StorageResult<LaneRecoveryReport> {
    let mut observer = SegmentObserver::new();
    recover_lane_at_root_inner(queue_root, lane, &mut observer)
}

/// Queue-internal consumer primitive: crash-safely reclaim the oldest consumed
/// sealed segment `seg_id` (`seg_id == min_live_seg && seg_id < active_seg &&
/// seg_id < highwater_seg`) under shared `append.lock` (`ADR-RB-05` #4 ③,
/// `RB-08j` AC7–AC8, `RB-09d`/`RB-09k`/`RB-09l`).
///
/// Restricted to `pub(super)` (`crate::storage::queue`) so only the single-writer
/// consumer (which holds `writer.lock` in global lock order
/// `writer.lock -> capture-queue/append.lock` and supplies the DB-authoritative
/// `QUEUE_HIGHWATER_*` segment id) and queue unit tests may invoke reclamation:
/// 1. Validates `seg_id < highwater_seg`, `seg_id == min_live_seg`,
///    `seg_id < active_seg`, `sealed[0].seg_id == seg_id`, and that
///    `events-<seg_id:08>.log` exists on disk as an owned non-symlink regular
///    file strictly BEFORE any Phase 2 metadata repair or `.tmp` cleanup.
/// 2. Atomically commits dual-replica `LTM2` (`tail.meta` + `tail.meta.bak`)
///    advancing `MIN_LIVE_SEG = seg_id + 1` and removing `seg_id` from `sealed[]`.
/// 3. Strictly after both `LTM2` replicas are persisted, re-verifies and unlinks
///    `events-<seg_id:08>.log` followed by `fsync_dir(lane_dir)`.
#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
pub(super) fn reclaim_sealed_segment(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    highwater_seg: u32,
) -> StorageResult<SegmentReclaimOutcome> {
    let mut observer = SegmentObserver::new();
    reclaim_sealed_segment_in_storage_dir(storage_dir, lane, seg_id, highwater_seg, &mut observer)
}

/// Queue-internal consumer primitive: crash-safely reclaim the oldest consumed
/// sealed segment `seg_id` under `<queue_root>/<lane>/` (`ADR-RB-05` #4 ③,
/// `RB-08j`). Callers must hold `writer.lock` and pass DB-authoritative
/// `highwater_seg`.
#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
pub(super) fn reclaim_sealed_segment_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    highwater_seg: u32,
) -> StorageResult<SegmentReclaimOutcome> {
    let mut observer = SegmentObserver::new();
    reclaim_sealed_segment_at_root_inner(queue_root, lane, seg_id, highwater_seg, &mut observer)
}

#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
fn reclaim_sealed_segment_in_storage_dir(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    highwater_seg: u32,
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentReclaimOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::reclaim_in_storage_dir_unix(
            storage_dir,
            lane,
            seg_id,
            highwater_seg,
            owners,
            observer,
        )
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::reclaim_in_storage_dir_non_unix(
            storage_dir,
            lane,
            seg_id,
            highwater_seg,
            observer,
        )
    }
}

#[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
fn reclaim_sealed_segment_at_root_inner(
    queue_root: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    highwater_seg: u32,
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentReclaimOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::reclaim_at_root_unix(queue_root, lane, seg_id, highwater_seg, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::reclaim_at_root_non_unix(queue_root, lane, seg_id, highwater_seg, observer)
    }
}

/// Read-only inspect `tail.meta` and `tail.meta.bak` inside `lane_dir` with
/// `O_NOFOLLOW` / UID checks and return the authoritative `LTM2` converged by
/// strict total order `(active_seg, committed_len, min_live_seg)`.
pub fn read_converged_lane_tail_meta(lane_dir: &Path) -> StorageResult<LaneTailMeta> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::read_converged_lane_tail_meta_unix(lane_dir, owners.root_uid)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::read_converged_lane_tail_meta_non_unix(lane_dir)
    }
}

/// Durably publish damaged uncommitted bytes into `quarantine/<id>.bin` via the
/// crash-safe 4-step barrier (`ADR-RB-05` #3, `RB-08t`):
/// 1. Create `quarantine/<id>.tmp` (`0600`, `O_NOFOLLOW`) and write `bytes`.
/// 2. `tmp_file.sync_data()`.
/// 3. Atomic `rename(<id>.tmp -> <id>.bin)`.
/// 4. `fsync_dir(quarantine_dir)`.
pub fn publish_quarantine_durable(
    quarantine_dir: &Path,
    id: &str,
    bytes: &[u8],
) -> StorageResult<QuarantinePublishReport> {
    let mut observer = SegmentObserver::new();
    publish_quarantine_durable_inner(quarantine_dir, id, bytes, &mut observer)
}

/// Crash-safe structural barrier for damaged segment tails (`ADR-RB-05` #3,
/// `RB-08t`): durably publishes `[valid_end, actual_len)` of
/// `<lane_dir>/events-<seg_id:08>.log` into `<quarantine_dir>/<id>.bin`
/// (`.tmp -> sync_data -> rename(.bin) -> fsync_dir(quarantine_dir)`) strictly
/// **before** truncating the segment to `valid_end` and calling `sync_data()`.
pub fn quarantine_and_truncate_segment(
    quarantine_dir: &Path,
    lane_dir: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    valid_end: u64,
) -> StorageResult<QuarantinePublishReport> {
    let mut observer = SegmentObserver::new();
    quarantine_and_truncate_segment_inner(
        quarantine_dir,
        lane_dir,
        lane,
        seg_id,
        valid_end,
        &mut observer,
    )
}

/// Crash-safe structural barrier for non-empty damaged orphan successor
/// segments (`ADR-RB-05` #3, `RB-08t`): durably publishes the full bytes of
/// `<lane_dir>/events-<orphan_seg_id:08>.log` into `<quarantine_dir>/<id>.bin`
/// (`.tmp -> sync_data -> rename(.bin) -> fsync_dir(quarantine_dir)`) strictly
/// **before** executing `unlink` + `fsync_dir(lane_dir)`.
pub fn quarantine_and_unlink_orphan(
    quarantine_dir: &Path,
    lane_dir: &Path,
    lane: CaptureQueueLane,
    orphan_seg_id: u32,
) -> StorageResult<QuarantinePublishReport> {
    let mut observer = SegmentObserver::new();
    quarantine_and_unlink_orphan_inner(quarantine_dir, lane_dir, lane, orphan_seg_id, &mut observer)
}

fn publish_quarantine_durable_inner(
    quarantine_dir: &Path,
    id: &str,
    bytes: &[u8],
    observer: &mut SegmentObserver,
) -> StorageResult<QuarantinePublishReport> {
    validate_quarantine_artifact_id(id)?;
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::publish_quarantine_durable_unix(quarantine_dir, id, bytes, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::publish_quarantine_durable_non_unix(quarantine_dir, id, bytes, observer)
    }
}

fn quarantine_and_truncate_segment_inner(
    quarantine_dir: &Path,
    lane_dir: &Path,
    lane: CaptureQueueLane,
    seg_id: u32,
    valid_end: u64,
    observer: &mut SegmentObserver,
) -> StorageResult<QuarantinePublishReport> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::quarantine_and_truncate_segment_unix(
            quarantine_dir,
            lane_dir,
            lane,
            seg_id,
            valid_end,
            owners,
            observer,
        )
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::quarantine_and_truncate_segment_non_unix(
            quarantine_dir,
            lane_dir,
            lane,
            seg_id,
            valid_end,
            observer,
        )
    }
}

fn quarantine_and_unlink_orphan_inner(
    quarantine_dir: &Path,
    lane_dir: &Path,
    lane: CaptureQueueLane,
    orphan_seg_id: u32,
    observer: &mut SegmentObserver,
) -> StorageResult<QuarantinePublishReport> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::quarantine_and_unlink_orphan_unix(
            quarantine_dir,
            lane_dir,
            lane,
            orphan_seg_id,
            owners,
            observer,
        )
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::quarantine_and_unlink_orphan_non_unix(
            quarantine_dir,
            lane_dir,
            lane,
            orphan_seg_id,
            observer,
        )
    }
}

/// Stream-compute the Castagnoli CRC32C of `file[start..start + len]` using a
/// fixed 64 KiB stack buffer (zero heap allocation, safe for arbitrarily large
/// damaged segments).
fn stream_crc32c_file_range(file: &mut File, start: u64, len: u64) -> StorageResult<u32> {
    extend_crc32c_over_file_range(file, start, len, 0)
}

/// Stream-copy exact `len` bytes from `src[start..start + len]` into `dst`
/// using a fixed 64 KiB stack buffer (zero heap allocation) while returning the
/// Castagnoli CRC32C of the exact bytes written.
fn stream_copy_file_range_with_crc(
    src: &mut File,
    start: u64,
    len: u64,
    dst: &mut File,
) -> StorageResult<u32> {
    src.seek(SeekFrom::Start(start))
        .map_err(|e| StorageBackendError::io("seek segment for streaming quarantine copy", e))?;

    let mut remaining = len;
    let mut running_crc = 0u32;
    let mut buf = [0u8; PREFIX_CRC_CHUNK_BYTES];

    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let read_n = read_exact_chunk(src, &mut buf[..want])
            .map_err(|e| StorageBackendError::io("read segment bytes for quarantine", e))?;
        if read_n < want {
            return Err(StorageBackendError::io(
                "short read streaming segment bytes for quarantine",
                io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof"),
            ));
        }
        dst.write_all(&buf[..read_n])
            .map_err(|e| StorageBackendError::io("write quarantine temp bytes", e))?;
        running_crc = crc32c_append(running_crc, &buf[..read_n]);
        remaining -= read_n as u64;
    }

    Ok(running_crc)
}

#[cfg(test)]
fn append_validated_frame_in_storage_dir(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    frame_buf: &[u8],
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentAppendOutcome> {
    append_validated_frame_in_storage_dir_with_options(
        storage_dir,
        lane,
        frame_buf,
        QueueAppendOptions::default(),
        observer,
    )
}

fn append_validated_frame_in_storage_dir_with_options(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    frame_buf: &[u8],
    options: QueueAppendOptions,
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentAppendOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::append_in_storage_dir_unix(
            storage_dir,
            lane,
            frame_buf,
            options,
            owners,
            observer,
        )
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::append_in_storage_dir_non_unix(
            storage_dir,
            lane,
            frame_buf,
            options,
            observer,
        )
    }
}

fn append_validated_frame_at_root_with_options(
    queue_root: &Path,
    lane: CaptureQueueLane,
    frame_buf: &[u8],
    options: QueueAppendOptions,
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentAppendOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::append_at_root_unix(queue_root, lane, frame_buf, options, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::append_at_root_non_unix(queue_root, lane, frame_buf, options, observer)
    }
}

fn recover_lane_in_storage_dir(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    observer: &mut SegmentObserver,
) -> StorageResult<LaneRecoveryReport> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::recover_in_storage_dir_unix(storage_dir, lane, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::recover_in_storage_dir_non_unix(storage_dir, lane, observer)
    }
}

fn recover_lane_at_root_inner(
    queue_root: &Path,
    lane: CaptureQueueLane,
    observer: &mut SegmentObserver,
) -> StorageResult<LaneRecoveryReport> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::recover_at_root_unix(queue_root, lane, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::recover_at_root_non_unix(queue_root, lane, observer)
    }
}

// ============================================================================
// Unix Descriptor-Pinned, O_NOFOLLOW, 0600, UID-Verified Implementation
// ============================================================================

#[cfg(unix)]
mod unix_seg {
    use super::*;

    fn verify_dir_mode_0700(st: &libc::stat, subject: &'static str) -> StorageResult<()> {
        if unix_perm_bits(st.st_mode) != QUEUE_DIR_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} permissions are not 0700"),
            });
        }
        Ok(())
    }

    fn lane_expected_uid(owners: UnixExpectedQueueOwners, lane: CaptureQueueLane) -> u32 {
        match lane {
            CaptureQueueLane::Priority => owners.priority_uid,
            CaptureQueueLane::Normal => owners.normal_uid,
        }
    }

    fn open_root_from_storage_dir(
        storage_dir: &Path,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<OwnedFd> {
        let storage_fd = open_ancestor_dir(storage_dir, "open repository storage directory")?;
        let (private_fd, private_st) = openat_dir_nofollow(
            &storage_fd,
            OsStr::new(PRIVATE_SUBDIR),
            owners.private_uid,
            "capture private directory",
        )?;
        verify_dir_mode_0700(&private_st, "capture private directory")?;
        let (root_fd, root_st) = openat_dir_nofollow(
            &private_fd,
            OsStr::new(CAPTURE_QUEUE_SUBDIR),
            owners.root_uid,
            "capture queue root directory",
        )?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        Ok(root_fd)
    }

    fn open_lane_dir_fd(
        root_fd: &OwnedFd,
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<OwnedFd> {
        let subject = match lane {
            CaptureQueueLane::Priority => "capture queue priority lane directory",
            CaptureQueueLane::Normal => "capture queue normal lane directory",
        };
        let (lane_fd, lane_st) = openat_dir_nofollow(
            root_fd,
            OsStr::new(lane_dir_name(lane)),
            lane_expected_uid(owners, lane),
            subject,
        )?;
        verify_dir_mode_0700(&lane_st, subject)?;
        Ok(lane_fd)
    }

    /// Acquire an exclusive `flock` on `lock_fd` without blocking past
    /// `deadline`, returning [`StorageBackendError::QueueBusy`] (`LBR-AGENT-049`)
    /// when contention outlives the caller's deadline (`ADR-RB-05` #3, `RB-08b`).
    fn flock_exclusive_with_deadline(
        lock_fd: &OwnedFd,
        deadline: Instant,
        context: &'static str,
    ) -> StorageResult<()> {
        acquire_lock_with_deadline_loop(
            deadline,
            context,
            || {
                // SAFETY: `lock_fd` is a valid open regular file descriptor.
                let rc = unsafe { libc::flock(lock_fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    return TryLockOutcome::Acquired;
                }
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    return TryLockOutcome::Retry;
                }
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return TryLockOutcome::WouldBlock;
                }
                TryLockOutcome::Error(err)
            },
            Instant::now,
            std::thread::sleep,
        )
    }

    /// Open an already-initialized `append.lock` strictly without `O_CREAT` or
    /// `fchmod`, verifying non-symlink, `S_IFREG`, effective UID, and exact
    /// `0600` mode, then acquire `flock(LOCK_EX)` bounded by `deadline`.
    ///
    /// Returns `Ok(None)` when `append.lock` is absent (`ENOENT`), letting the
    /// caller run read-only Phase 1 classification before deciding whether to
    /// create `append.lock` so fail-closed classifications leave disk 100%
    /// unchanged.
    pub(super) fn try_acquire_existing_strict_append_lock_unix(
        root_fd: &OwnedFd,
        expected_uid: u32,
        deadline: Instant,
    ) -> StorageResult<Option<AppendLockGuard>> {
        let cname = osstr_to_cstring(
            OsStr::new(QUEUE_APPEND_LOCK_FILE),
            "open capture queue append.lock",
        )?;
        let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(root_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            if is_symlink_errno_at(&err, root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue append.lock"));
            }
            return Err(StorageBackendError::io(
                "open capture queue append.lock",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let lock_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&lock_fd, "stat capture queue append.lock")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue append.lock is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue append.lock"));
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue append.lock permissions are not 0600".to_string(),
            });
        }
        flock_exclusive_with_deadline(&lock_fd, deadline, "lock capture queue append.lock")?;
        Ok(Some(AppendLockGuard::from_fd(lock_fd)))
    }

    /// Exclusively create `append.lock` (`O_CREAT | O_EXCL | O_NOFOLLOW`) and
    /// apply `fchmod(0600)` ONLY to the inode this call created. If a racing
    /// actor created `append.lock` first (`AlreadyExists`), immediately delegate
    /// to [`try_acquire_existing_strict_append_lock_unix`], which verifies exact
    /// `0600` mode and fails closed WITHOUT `fchmod` if the racing file is
    /// permissive.
    fn create_or_acquire_strict_append_lock_unix(
        root_fd: &OwnedFd,
        expected_uid: u32,
        deadline: Instant,
    ) -> StorageResult<AppendLockGuard> {
        let cname = osstr_to_cstring(
            OsStr::new(QUEUE_APPEND_LOCK_FILE),
            "create capture queue append.lock",
        )?;
        let create_excl_flags =
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;

        for _ in 0..32 {
            // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
            let raw_created = unsafe {
                libc::openat(
                    root_fd.as_raw_fd(),
                    cname.as_ptr(),
                    create_excl_flags,
                    QUEUE_FILE_MODE as libc::c_uint,
                )
            };
            if raw_created >= 0 {
                // SAFETY: `raw_created >= 0` is exclusively owned and newly created by us.
                let lock_fd = unsafe { OwnedFd::from_raw_fd(raw_created) };
                let st = fstat_fd(&lock_fd, "stat newly created capture queue append.lock")?;
                if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "capture queue append.lock is not a regular file".to_string(),
                    });
                }
                if st.st_uid != expected_uid {
                    return Err(map_queue_owner_mismatch_err("capture queue append.lock"));
                }
                if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
                    fchmod_fd(
                        &lock_fd,
                        QUEUE_FILE_MODE,
                        "enforce 0600 mode on newly created capture queue append.lock",
                    )?;
                    let verified = fstat_fd(
                        &lock_fd,
                        "verify newly created capture queue append.lock mode",
                    )?;
                    if unix_perm_bits(verified.st_mode) != QUEUE_FILE_MODE {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "capture queue append.lock permissions failed to settle at 0600"
                                    .to_string(),
                        });
                    }
                }
                flock_exclusive_with_deadline(
                    &lock_fd,
                    deadline,
                    "lock newly created capture queue append.lock",
                )?;
                fsync_fd(
                    root_fd,
                    "fsync capture queue root after creating append.lock",
                )?;
                return Ok(AppendLockGuard::from_fd(lock_fd));
            }

            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue append.lock"));
            }
            if err.kind() == io::ErrorKind::AlreadyExists || err.kind() == io::ErrorKind::NotFound {
                if let Some(lock) =
                    try_acquire_existing_strict_append_lock_unix(root_fd, expected_uid, deadline)?
                {
                    return Ok(lock);
                }
                continue;
            }
            return Err(StorageBackendError::io(
                "create capture queue append.lock",
                err,
            ));
        }

        Err(StorageBackendError::BackendInternal {
            detail: "capture queue append.lock open/create contended past retry bound".to_string(),
        })
    }

    /// Acquire `append.lock` bounded by `lock_timeout` and execute Phase 1
    /// strictly read-only classification without ever mutating disk before a
    /// fail-closed error:
    /// - If `append.lock` exists with exact `0600`, lock it immediately (zero
    ///   mutation) and classify Phase 1 under lock.
    /// - If `append.lock` is permissive (`!= 0600`), fail closed without
    ///   `fchmod`.
    /// - If `append.lock` is absent, run read-only Phase 1 FIRST so corrupt /
    ///   fail-closed lanes leave zero files created (not even `append.lock`);
    ///   only if Phase 1 classifies the lane as valid/recoverable do we
    ///   exclusively create `append.lock` (`0600`) — failing closed without
    ///   `fchmod` if a racer planted a permissive lock in the interval — and
    ///   re-classify Phase 1 under the held lock.
    fn acquire_lock_and_phase1_unix(
        root_fd: &OwnedFd,
        lane_fd: &OwnedFd,
        owners: UnixExpectedQueueOwners,
        lane_uid: u32,
        lock_timeout: Duration,
        observer: &mut SegmentObserver,
    ) -> StorageResult<(AppendLockGuard, Phase1Plan<OsString>)> {
        let deadline = lock_deadline_for_timeout(lock_timeout);
        if let Some(lock) =
            try_acquire_existing_strict_append_lock_unix(root_fd, owners.root_uid, deadline)?
        {
            observer.record_acquire_lock();
            let plan = phase1_classify_lane_unix(lane_fd, lane_uid)?;
            return Ok((lock, plan));
        }

        // `append.lock` is absent: pre-classify strictly read-only so any
        // fail-closed state exits with zero filesystem mutation.
        let _ = phase1_classify_lane_unix(lane_fd, lane_uid)?;
        observer.on_before_create_missing_lock();

        let lock = create_or_acquire_strict_append_lock_unix(root_fd, owners.root_uid, deadline)?;
        observer.record_acquire_lock();
        let plan = phase1_classify_lane_unix(lane_fd, lane_uid)?;
        Ok((lock, plan))
    }

    struct LaneDirScan {
        seg_ids: Vec<u32>,
        stale_tmp_names: Vec<OsString>,
    }

    fn scan_lane_dir_readonly(lane_fd: &OwnedFd) -> StorageResult<LaneDirScan> {
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
        // SAFETY: `guard.ptr` is a valid open `DIR*`; rewind because
        // `F_DUPFD_CLOEXEC` shares the open file description's directory offset.
        unsafe {
            libc::rewinddir(guard.ptr);
        }
        let mut seg_ids = Vec::new();
        let mut stale_tmp_names = Vec::new();

        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is a valid open `DIR*`.
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
            } else if is_lane_meta_temp_name(name_bytes) {
                stale_tmp_names.push(OsStr::from_bytes(name_bytes).to_os_string());
            }
        }

        seg_ids.sort_unstable();
        seg_ids.dedup();
        Ok(LaneDirScan {
            seg_ids,
            stale_tmp_names,
        })
    }

    fn read_meta_replica_nofollow(
        lane_fd: &OwnedFd,
        file_name: &str,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<ReplicaState> {
        let cname = osstr_to_cstring(OsStr::new(file_name), "open capture queue metadata")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(ReplicaState::Absent);
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
            return Ok(ReplicaState::Corrupt);
        };
        if file_len > LTM2_MAX_LEN as u64 {
            return Ok(ReplicaState::Corrupt);
        }
        let bytes =
            read_bounded_fd_with_context(&fd, LTM2_MAX_LEN + 1, "read capture queue metadata")?;
        if bytes.len() > LTM2_MAX_LEN {
            return Ok(ReplicaState::Corrupt);
        }
        match LaneTailMeta::decode(&bytes) {
            Ok(meta) => Ok(ReplicaState::Valid(meta)),
            Err(_) => Ok(ReplicaState::Corrupt),
        }
    }

    fn open_segment_readonly_nofollow(
        lane_fd: &OwnedFd,
        seg_id: u32,
        expected_uid: u32,
    ) -> StorageResult<(File, u64)> {
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open capture queue segment")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            if err.kind() == io::ErrorKind::NotFound {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "Case E: active capture queue segment file is missing".to_string(),
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

    /// Strictly 100% read-only Phase 1 lane classification under held
    /// `append.lock` (`ADR-RB-05` #3 Step ①). Performs ZERO mutations to disk.
    fn phase1_classify_lane_unix(
        lane_fd: &OwnedFd,
        expected_uid: u32,
    ) -> StorageResult<Phase1Plan<OsString>> {
        let scan = scan_lane_dir_readonly(lane_fd)?;
        let primary = read_meta_replica_nofollow(
            lane_fd,
            QUEUE_TAIL_META_FILE,
            expected_uid,
            "capture queue tail.meta",
        )?;
        let backup = read_meta_replica_nofollow(
            lane_fd,
            QUEUE_TAIL_META_BAK_FILE,
            expected_uid,
            "capture queue tail.meta.bak",
        )?;
        let resolved = resolve_dual_replicas(primary, backup)?;

        if scan.seg_ids.is_empty() {
            return match resolved {
                ResolvedReplicas::BothAbsent => Ok(Phase1Plan::Case0aFreshEmpty {
                    stale_tmp_names: scan.stale_tmp_names,
                }),
                ResolvedReplicas::Authoritative { meta, mirror_sync } => {
                    if meta.is_initial_empty() {
                        Ok(Phase1Plan::Case0bPreInitializedEmpty {
                            meta,
                            mirror_sync,
                            stale_tmp_names: scan.stale_tmp_names,
                        })
                    } else {
                        Err(StorageBackendError::CorruptCommittedFrame {
                            detail:
                                "Case 0c/E: zero segment files present while LTM2 records non-empty committed state"
                                    .to_string(),
                        })
                    }
                }
                ResolvedReplicas::BothInvalidWithPresence => {
                    Err(StorageBackendError::CorruptCommittedFrame {
                        detail:
                            "Case 0d: zero segment files present with corrupt tail.meta replicas"
                                .to_string(),
                    })
                }
            };
        }

        let (meta, mirror_sync) = match resolved {
            ResolvedReplicas::Authoritative { meta, mirror_sync } => (meta, mirror_sync),
            ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail:
                        "Subcase B2: segment files exist while both LTM2 tail.meta replicas are absent or corrupt"
                            .to_string(),
                });
            }
        };

        let mut reclaimed_orphans = Vec::new();
        let mut live_segs = Vec::new();
        for seg_id in scan.seg_ids {
            if seg_id < meta.min_live_seg {
                reclaimed_orphans.push(seg_id);
            } else {
                live_segs.push(seg_id);
            }
        }

        let Some(&max_live_seg) = live_segs.last() else {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "Case E: no live segment files >= min_live_seg present".to_string(),
            });
        };
        if max_live_seg < meta.active_seg || live_segs.binary_search(&meta.active_seg).is_err() {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail:
                    "Case E: active segment missing or max live segment regressed below active_seg"
                        .to_string(),
            });
        }

        let (mut ro_seg, actual_len) =
            open_segment_readonly_nofollow(lane_fd, meta.active_seg, expected_uid)?;
        if actual_len < meta.committed_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "Case E: active segment file is shorter than LTM2 committed_len"
                    .to_string(),
            });
        }

        verify_committed_prefix_crc(&mut ro_seg, meta.committed_len, meta.prefix_crc32c)?;

        if max_live_seg > meta.active_seg {
            let succ_seg = validate_single_case_d_successor_id(meta.active_seg, &live_segs)?;
            let (mut succ_file, succ_actual_len) =
                open_segment_readonly_nofollow(lane_fd, succ_seg, expected_uid)?;
            let succ_scan = scan_successor_segment_readonly(&mut succ_file, succ_actual_len)?;
            ensure_case_d_sealed_capacity(&meta, &succ_scan)?;
            let old_tail = if actual_len > meta.committed_len {
                let tail = scan_case_c_uncommitted_tail(
                    &mut ro_seg,
                    meta.committed_len,
                    actual_len,
                    meta.prefix_crc32c,
                )?;
                Some(NestedOldTailPlan {
                    valid_end: tail.valid_end,
                    recovered_crc32c: tail.recovered_crc32c,
                    frame_count: tail.frame_count,
                    has_damaged_tail: tail.has_damaged_tail,
                })
            } else {
                None
            };
            return match classify_case_d_readonly(
                &meta,
                old_tail.as_ref(),
                succ_actual_len,
                &succ_scan,
            )? {
                CaseDPhase1Kind::ValidSuccessor { has_damaged_tail } => {
                    Ok(Phase1Plan::CaseDRecoverValidSuccessor {
                        meta,
                        old_tail,
                        succ_seg,
                        succ_valid_end: succ_scan.valid_end,
                        succ_crc32c: succ_scan.valid_crc32c,
                        succ_frames: succ_scan.frame_count,
                        succ_has_damaged_tail: has_damaged_tail,
                        stale_tmp_names: scan.stale_tmp_names,
                        reclaimed_orphans,
                    })
                }
                CaseDPhase1Kind::OrphanSuccessor { non_empty } => {
                    Ok(Phase1Plan::CaseDCleanOrphanSuccessor {
                        meta,
                        old_tail,
                        orphan_seg: succ_seg,
                        orphan_non_empty: non_empty,
                        stale_tmp_names: scan.stale_tmp_names,
                        reclaimed_orphans,
                    })
                }
            };
        }

        if actual_len == meta.committed_len {
            Ok(Phase1Plan::CaseAClean {
                meta,
                mirror_sync,
                stale_tmp_names: scan.stale_tmp_names,
                reclaimed_orphans,
            })
        } else {
            let tail = scan_case_c_uncommitted_tail(
                &mut ro_seg,
                meta.committed_len,
                actual_len,
                meta.prefix_crc32c,
            )?;
            Ok(Phase1Plan::CaseCRecoverTail {
                meta,
                recovered_valid_end: tail.valid_end,
                recovered_crc32c: tail.recovered_crc32c,
                recovered_frames: tail.frame_count,
                has_damaged_tail: tail.has_damaged_tail,
                stale_tmp_names: scan.stale_tmp_names,
                reclaimed_orphans,
            })
        }
    }

    fn clean_stale_tmp_files_unix(
        lane_fd: &OwnedFd,
        stale_tmp_names: &[OsString],
        expected_uid: u32,
    ) -> StorageResult<usize> {
        let mut removed = 0usize;
        for tmp_name in stale_tmp_names {
            let cname = osstr_to_cstring(tmp_name.as_os_str(), "clean stale capture queue temp")?;
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    lane_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(StorageBackendError::io(
                    "inspect stale capture queue temp metadata",
                    err,
                ));
            }
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue temp metadata"));
            }
            if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "stale capture queue temp entry is not a regular file".to_string(),
                });
            }
            if st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err("capture queue temp metadata"));
            }
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let unlink_rc = unsafe { libc::unlinkat(lane_fd.as_raw_fd(), cname.as_ptr(), 0) };
            if unlink_rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(StorageBackendError::io(
                        "unlink stale capture queue temp metadata",
                        err,
                    ));
                }
            } else {
                removed += 1;
            }
        }
        if removed > 0 {
            fsync_fd(
                lane_fd,
                "fsync capture queue lane after cleaning temp files",
            )?;
        }
        Ok(removed)
    }

    fn clean_reclaimed_orphans_unix(
        lane_fd: &OwnedFd,
        reclaimed_orphans: &[u32],
        expected_uid: u32,
    ) -> StorageResult<usize> {
        let mut removed = 0usize;
        for &seg_id in reclaimed_orphans {
            let name = segment_log_file_name(seg_id);
            let cname =
                osstr_to_cstring(OsStr::new(&name), "unlink reclaimed capture queue orphan")?;
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    lane_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(StorageBackendError::io(
                    "inspect reclaimed capture queue orphan",
                    err,
                ));
            }
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue reclaimed orphan"));
            }
            if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "reclaimed capture queue orphan is not a regular file".to_string(),
                });
            }
            if st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err(
                    "capture queue reclaimed orphan",
                ));
            }
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let unlink_rc = unsafe { libc::unlinkat(lane_fd.as_raw_fd(), cname.as_ptr(), 0) };
            if unlink_rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(StorageBackendError::io(
                        "unlink reclaimed capture queue orphan",
                        err,
                    ));
                }
            } else {
                removed += 1;
            }
        }
        if removed > 0 {
            fsync_fd(
                lane_fd,
                "fsync capture queue lane after unlinking reclaimed orphans",
            )?;
        }
        Ok(removed)
    }

    /// Atomically replace a single `LTM2` metadata replica (`tail.meta` or
    /// `tail.meta.bak`) via its canonical staging file (`tail.meta.tmp` or
    /// `tail.meta.bak.tmp`):
    /// `openat(O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600)` ->
    /// `fchmod(0600)` + verify -> `write_all` -> `sync_data()` ->
    /// `renameat(tmp, target)` -> `fsync_dir(lane_fd)`.
    fn commit_single_meta_replica_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        tmp_name: &'static str,
        target_name: &'static str,
        encoded_bytes: &[u8],
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<()> {
        let ctmp = osstr_to_cstring(OsStr::new(tmp_name), "create capture queue temp meta")?;
        let ctarget = osstr_to_cstring(OsStr::new(target_name), "replace capture queue meta")?;

        // Refuse to overwrite a symlink planted at `target_name`.
        let mut target_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `lane_fd` is valid and `ctarget` is NUL-terminated.
        let target_rc = unsafe {
            libc::fstatat(
                lane_fd.as_raw_fd(),
                ctarget.as_ptr(),
                target_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if target_rc == 0 {
            // SAFETY: `fstatat` succeeded.
            let target_st = unsafe { target_st.assume_init() };
            if (target_st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue tail metadata"));
            }
            if (target_st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture queue tail metadata target is not a regular file".to_string(),
                });
            }
            if target_st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err("capture queue tail metadata"));
            }
        } else {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "inspect capture queue tail metadata before atomic replace",
                    err,
                ));
            }
        }

        let create_flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `lane_fd` is valid and `ctmp` is NUL-terminated.
        let raw_fd = unsafe {
            libc::openat(
                lane_fd.as_raw_fd(),
                ctmp.as_ptr(),
                create_flags,
                QUEUE_FILE_MODE as libc::c_uint,
            )
        };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &ctmp) {
                return Err(map_queue_symlink_err("capture queue temp metadata"));
            }
            return Err(StorageBackendError::io(
                "create capture queue temp metadata",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let tmp_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut unlink_guard = TempUnlinkGuard::new(lane_fd, ctmp.clone());

        fchmod_fd(
            &tmp_fd,
            QUEUE_FILE_MODE,
            "enforce 0600 mode on capture queue temp metadata",
        )?;
        let st = fstat_fd(&tmp_fd, "verify capture queue temp metadata mode")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue temp metadata is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue temp metadata"));
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue temp metadata permissions failed to settle at 0600"
                    .to_string(),
            });
        }

        let mut tmp_file = File::from(tmp_fd);
        tmp_file
            .write_all(encoded_bytes)
            .map_err(|e| StorageBackendError::io("write LTM2 temp metadata", e))?;
        tmp_file
            .sync_data()
            .map_err(|e| StorageBackendError::io("sync LTM2 temp metadata", e))?;
        observer.record_meta_tmp_synced(lane_label, tmp_name, target_name);

        let before_rename_crash = if target_name == QUEUE_TAIL_META_FILE {
            SegmentCrashPoint::BeforePrimaryMetaRename
        } else {
            SegmentCrashPoint::BeforeBackupMetaRename
        };
        if let Err(err) = observer.maybe_crash(before_rename_crash) {
            // Preserve `.tmp` on disk to faithfully model `SIGKILL` before rename.
            unlink_guard.disarm();
            drop(tmp_file);
            return Err(err);
        }

        // SAFETY: `lane_fd` is valid and both CStrings are NUL-terminated.
        let rename_rc = unsafe {
            libc::renameat(
                lane_fd.as_raw_fd(),
                ctmp.as_ptr(),
                lane_fd.as_raw_fd(),
                ctarget.as_ptr(),
            )
        };
        if rename_rc != 0 {
            return Err(StorageBackendError::io(
                "atomically replace capture queue LTM2 metadata",
                io::Error::last_os_error(),
            ));
        }
        unlink_guard.disarm();
        drop(tmp_file);
        observer.record_meta_renamed(lane_label, tmp_name, target_name);

        fsync_fd(
            lane_fd,
            "fsync capture queue lane directory after metadata replace",
        )?;
        observer.record_fsync_lane_after_meta(lane_label, target_name);

        if target_name == QUEUE_TAIL_META_FILE {
            observer.maybe_crash(SegmentCrashPoint::AfterPrimaryMetaCommit)?;
        }
        Ok(())
    }

    /// Commit BOTH `tail.meta` and `tail.meta.bak` in strict sequence under
    /// held `append.lock` (`ADR-RB-05` #3, `RB-08` AC3–AC4).
    fn commit_dual_tail_meta_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        meta: &LaneTailMeta,
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<()> {
        let encoded = meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
        commit_single_meta_replica_unix(
            lane_fd,
            lane_label,
            QUEUE_TAIL_META_TMP_FILE,
            QUEUE_TAIL_META_FILE,
            &encoded,
            expected_uid,
            observer,
        )?;
        commit_single_meta_replica_unix(
            lane_fd,
            lane_label,
            QUEUE_TAIL_META_BAK_TMP_FILE,
            QUEUE_TAIL_META_BAK_FILE,
            &encoded,
            expected_uid,
            observer,
        )?;
        Ok(())
    }

    struct Phase2ConvergedState {
        meta: LaneTailMeta,
        active_seg_exists: bool,
        recovered_frames: usize,
        recovered_bytes: u64,
        mirrored_replica: bool,
        cleaned_tmp_files: usize,
        reclaimed_orphans_removed: usize,
        quarantined_artifacts: Vec<QuarantinePublishReport>,
        lane_dir_synced: bool,
    }

    struct UnixPhase2Context<'a> {
        lane: CaptureQueueLane,
        lane_fd: &'a OwnedFd,
        quarantine_fd: &'a OwnedFd,
        quarantine_dir: &'a Path,
        owners: UnixExpectedQueueOwners,
        append_lock: &'a AppendLockGuard,
    }

    struct CaseCStepC1Outcome {
        meta: LaneTailMeta,
        recovered_bytes: u64,
        quarantined_artifact: Option<QuarantinePublishReport>,
    }

    /// Shared `ADR-RB-05` #3 Case C Step C1 production routine (`RB-08m`):
    /// - If `has_damaged_tail` is `true`, calls `RB-08t::quarantine_and_truncate_segment_at_fds`
    ///   to durably publish `[recovered_valid_end, actual_len)` to `quarantine/`
    ///   (`.tmp -> sync_data -> rename(.bin) -> fsync_dir(quarantine_dir)`,
    ///   recording `LBR-AGENT-050`) strictly BEFORE truncating
    ///   `events-<S>.log` to exact `recovered_valid_end` and calling `sync_data()`.
    /// - Atomically commits dual-replica `LTM2` (`tail.meta` + `tail.meta.bak`)
    ///   updating `COMMITTED_LEN = recovered_valid_end` and
    ///   `PREFIX_CRC32C = recovered_crc32c` while preserving `ACTIVE_SEG == S`
    ///   and `sealed[]` unchanged before any `S + 1` file is created.
    fn case_c_step_c1_unix(
        ctx: &UnixPhase2Context<'_>,
        meta: LaneTailMeta,
        recovered_valid_end: u64,
        recovered_crc32c: u32,
        recovered_frames: usize,
        has_damaged_tail: bool,
        observer: &mut SegmentObserver,
    ) -> StorageResult<CaseCStepC1Outcome> {
        let lane_fd = ctx.lane_fd;
        let lane_label = lane_dir_name(ctx.lane);
        let expected_uid = lane_expected_uid(ctx.owners, ctx.lane);

        let recovered_bytes = recovered_valid_end
            .checked_sub(meta.committed_len)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "case C recovered length underflow".to_string(),
            })?;

        let quarantined_artifact = if has_damaged_tail {
            let q_dirs = UnixQuarantineDirs {
                quarantine_fd: ctx.quarantine_fd,
                quarantine_dir: ctx.quarantine_dir,
                lane_fd,
                owners: ctx.owners,
                append_lock: ctx.append_lock,
            };
            let report = quarantine_and_truncate_segment_at_fds(
                &q_dirs,
                ctx.lane,
                meta.active_seg,
                recovered_valid_end,
                observer,
            )?;
            observer.maybe_crash(SegmentCrashPoint::AfterCaseCStepC1TruncateSyncBeforeMeta)?;
            Some(report)
        } else {
            None
        };

        observer.record_case_c_recover(
            lane_label,
            meta.active_seg,
            meta.committed_len,
            recovered_valid_end,
            recovered_frames,
        );
        let next_meta = LaneTailMeta {
            min_live_seg: meta.min_live_seg,
            active_seg: meta.active_seg,
            committed_len: recovered_valid_end,
            prefix_crc32c: recovered_crc32c,
            sealed: meta.sealed,
        };
        commit_dual_tail_meta_unix(lane_fd, lane_label, &next_meta, expected_uid, observer)?;

        Ok(CaseCStepC1Outcome {
            meta: next_meta,
            recovered_bytes,
            quarantined_artifact,
        })
    }

    /// Base Unix `ADR-RB-05` #3 Case C **Step C2** production routine (`RB-08n`
    /// AC1–AC5): after `case_c_step_c1_unix` has quarantined and truncated a
    /// damaged tail and committed Step C1 dual `LTM2`, validate and build the
    /// sealed-transition `LTM2`, reuse the shared locked
    /// `create_segment_file_at_fd_unix` primitive (`0600` + `fsync_dir`) to
    /// create `events-<S+1:08>.log`, and atomically commit dual-replica `LTM2`
    /// sealing `(S, recovered_valid_end, recovered_crc32c)` into `sealed[]`,
    /// advancing `ACTIVE_SEG = S + 1`, and resetting `COMMITTED_LEN = 0` and
    /// `PREFIX_CRC32C = 0`.
    fn seal_recovered_segment_to_next_unix(
        ctx: &UnixPhase2Context<'_>,
        step_c1_meta: &LaneTailMeta,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneTailMeta> {
        let next_meta = build_case_c_step_c2_meta(step_c1_meta)?;
        let lane_label = lane_dir_name(ctx.lane);
        let expected_uid = lane_expected_uid(ctx.owners, ctx.lane);
        let _new_seg_file = create_segment_file_at_fd_unix(
            ctx.lane_fd,
            lane_label,
            next_meta.active_seg,
            expected_uid,
            ctx.append_lock,
            observer,
        )?;
        observer.maybe_crash(SegmentCrashPoint::AfterStepC2CreateSegmentBeforeMeta)?;
        commit_dual_tail_meta_unix(ctx.lane_fd, lane_label, &next_meta, expected_uid, observer)?;
        Ok(next_meta)
    }

    fn phase2_execute_plan_unix(
        ctx: &UnixPhase2Context<'_>,
        plan: Phase1Plan<OsString>,
        observer: &mut SegmentObserver,
    ) -> StorageResult<Phase2ConvergedState> {
        let lane_fd = ctx.lane_fd;
        let lane_label = lane_dir_name(ctx.lane);
        let expected_uid = lane_expected_uid(ctx.owners, ctx.lane);
        match plan {
            Phase1Plan::Case0aFreshEmpty { stale_tmp_names } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let meta = LaneTailMeta::initial_empty();
                commit_dual_tail_meta_unix(lane_fd, lane_label, &meta, expected_uid, observer)?;
                Ok(Phase2ConvergedState {
                    meta,
                    active_seg_exists: false,
                    recovered_frames: 0,
                    recovered_bytes: 0,
                    mirrored_replica: false,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed: 0,
                    quarantined_artifacts: Vec::new(),
                    lane_dir_synced: true,
                })
            }
            Phase1Plan::Case0bPreInitializedEmpty {
                meta,
                mirror_sync,
                stale_tmp_names,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let mirrored_replica = apply_mirror_sync_unix(
                    lane_fd,
                    lane_label,
                    &meta,
                    mirror_sync,
                    expected_uid,
                    observer,
                )?;
                Ok(Phase2ConvergedState {
                    meta,
                    active_seg_exists: false,
                    recovered_frames: 0,
                    recovered_bytes: 0,
                    mirrored_replica,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed: 0,
                    quarantined_artifacts: Vec::new(),
                    lane_dir_synced: mirrored_replica || cleaned_tmp_files > 0,
                })
            }
            Phase1Plan::CaseAClean {
                meta,
                mirror_sync,
                stale_tmp_names,
                reclaimed_orphans,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let reclaimed_orphans_removed =
                    clean_reclaimed_orphans_unix(lane_fd, &reclaimed_orphans, expected_uid)?;
                let mirrored_replica = apply_mirror_sync_unix(
                    lane_fd,
                    lane_label,
                    &meta,
                    mirror_sync,
                    expected_uid,
                    observer,
                )?;
                Ok(Phase2ConvergedState {
                    meta,
                    active_seg_exists: true,
                    recovered_frames: 0,
                    recovered_bytes: 0,
                    mirrored_replica,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed,
                    quarantined_artifacts: Vec::new(),
                    lane_dir_synced: mirrored_replica
                        || cleaned_tmp_files > 0
                        || reclaimed_orphans_removed > 0,
                })
            }
            Phase1Plan::CaseCRecoverTail {
                meta,
                recovered_valid_end,
                recovered_crc32c,
                recovered_frames,
                has_damaged_tail,
                stale_tmp_names,
                reclaimed_orphans,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let reclaimed_orphans_removed =
                    clean_reclaimed_orphans_unix(lane_fd, &reclaimed_orphans, expected_uid)?;
                let step_c1 = case_c_step_c1_unix(
                    ctx,
                    meta,
                    recovered_valid_end,
                    recovered_crc32c,
                    recovered_frames,
                    has_damaged_tail,
                    observer,
                )?;
                // `RB-08q`: Skip Step C2 when `recovered_valid_end == 0` (fresh
                // segment first-frame crash) so we keep `ACTIVE_SEG == S` and
                // never write a 0-byte sealed entry into `LTM2.sealed[]`.
                let final_meta = if has_damaged_tail && step_c1.meta.committed_len > 0 {
                    observer.maybe_crash(SegmentCrashPoint::AfterCaseCStepC1MetaBeforeStepC2)?;
                    seal_recovered_segment_to_next_unix(ctx, &step_c1.meta, observer)?
                } else {
                    step_c1.meta
                };
                let quarantined_artifacts = step_c1.quarantined_artifact.into_iter().collect();
                Ok(Phase2ConvergedState {
                    meta: final_meta,
                    active_seg_exists: true,
                    recovered_frames,
                    recovered_bytes: step_c1.recovered_bytes,
                    mirrored_replica: true,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed,
                    quarantined_artifacts,
                    lane_dir_synced: true,
                })
            }
            Phase1Plan::CaseDRecoverValidSuccessor {
                meta,
                old_tail,
                succ_seg,
                succ_valid_end,
                succ_crc32c,
                succ_frames,
                succ_has_damaged_tail,
                stale_tmp_names,
                reclaimed_orphans,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let reclaimed_orphans_removed =
                    clean_reclaimed_orphans_unix(lane_fd, &reclaimed_orphans, expected_uid)?;
                let mut quarantined_artifacts = Vec::new();
                let (effective_old_meta, old_recovered_frames, old_recovered_bytes) =
                    if let Some(ot) = old_tail {
                        // `RB-08k` / `RB-08v` (`ADR-RB-05` #3 Case D): recover
                        // nested old-segment uncommitted tail via Step C1 BEFORE
                        // sealing `(S, old_valid_end, old_sealed_crc)` into `LTM2.sealed[]`.
                        let step_c1 = case_c_step_c1_unix(
                            ctx,
                            meta,
                            ot.valid_end,
                            ot.recovered_crc32c,
                            ot.frame_count,
                            ot.has_damaged_tail,
                            observer,
                        )?;
                        if let Some(q_rep) = step_c1.quarantined_artifact {
                            quarantined_artifacts.push(q_rep);
                        }
                        (step_c1.meta, ot.frame_count, step_c1.recovered_bytes)
                    } else {
                        (meta, 0, 0)
                    };
                if succ_has_damaged_tail {
                    // `RB-08u` AC1–AC7: mandatory durable quarantine barrier via
                    // `RB-08t::quarantine_and_truncate_segment_at_fds` BEFORE
                    // truncating `events-<S+1>.log` to `succ_valid_end` + `sync_data()`.
                    let q_dirs = UnixQuarantineDirs {
                        quarantine_fd: ctx.quarantine_fd,
                        quarantine_dir: ctx.quarantine_dir,
                        lane_fd,
                        owners: ctx.owners,
                        append_lock: ctx.append_lock,
                    };
                    let q_report = quarantine_and_truncate_segment_at_fds(
                        &q_dirs,
                        ctx.lane,
                        succ_seg,
                        succ_valid_end,
                        observer,
                    )?;
                    quarantined_artifacts.push(q_report);
                }
                let next_meta = build_case_d_sealed_successor_meta(
                    &effective_old_meta,
                    succ_seg,
                    succ_valid_end,
                    succ_crc32c,
                )?;
                observer.record_case_d_recover_successor(
                    lane_label,
                    effective_old_meta.active_seg,
                    succ_seg,
                    succ_valid_end,
                    succ_frames,
                );
                commit_dual_tail_meta_unix(
                    lane_fd,
                    lane_label,
                    &next_meta,
                    expected_uid,
                    observer,
                )?;
                Ok(Phase2ConvergedState {
                    meta: next_meta,
                    active_seg_exists: true,
                    recovered_frames: old_recovered_frames + succ_frames,
                    recovered_bytes: old_recovered_bytes + succ_valid_end,
                    mirrored_replica: true,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed,
                    quarantined_artifacts,
                    lane_dir_synced: true,
                })
            }
            Phase1Plan::CaseDCleanOrphanSuccessor {
                meta,
                old_tail,
                orphan_seg,
                orphan_non_empty,
                stale_tmp_names,
                reclaimed_orphans,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let reclaimed_orphans_removed =
                    clean_reclaimed_orphans_unix(lane_fd, &reclaimed_orphans, expected_uid)?;
                let mut quarantined_artifacts = Vec::new();
                let (effective_old_meta, old_recovered_frames, old_recovered_bytes) =
                    if let Some(ot) = old_tail {
                        let step_c1 = case_c_step_c1_unix(
                            ctx,
                            meta,
                            ot.valid_end,
                            ot.recovered_crc32c,
                            ot.frame_count,
                            ot.has_damaged_tail,
                            observer,
                        )?;
                        if let Some(q_rep) = step_c1.quarantined_artifact {
                            quarantined_artifacts.push(q_rep);
                        }
                        (step_c1.meta, ot.frame_count, step_c1.recovered_bytes)
                    } else {
                        (meta, 0, 0)
                    };
                if orphan_non_empty {
                    // `RB-08f` AC3–AC5, AC7–AC8: non-empty half-frame orphan MUST
                    // go through `RB-08t::quarantine_and_unlink_orphan_at_fds` so
                    // `publish_quarantine_durable` completes `fsync_dir(quarantine_dir)`
                    // strictly before `unlink(orphan)` + `fsync_dir(lane_dir)`.
                    let q_dirs = UnixQuarantineDirs {
                        quarantine_fd: ctx.quarantine_fd,
                        quarantine_dir: ctx.quarantine_dir,
                        lane_fd,
                        owners: ctx.owners,
                        append_lock: ctx.append_lock,
                    };
                    let q_report = quarantine_and_unlink_orphan_at_fds(
                        &q_dirs, ctx.lane, orphan_seg, observer,
                    )?;
                    quarantined_artifacts.push(q_report);
                } else {
                    // `RB-08f` AC1–AC2: empty 0-byte orphan -> inode-pinned `unlink`
                    // + `fsync_dir(lane_dir)` (zero quarantine artifact).
                    unlink_empty_orphan_at_fd_unix(
                        lane_fd,
                        ctx.lane,
                        orphan_seg,
                        expected_uid,
                        observer,
                    )?;
                }
                // `RB-08f` AC6: sync both `tail.meta` and `tail.meta.bak` to intact
                // `active_seg` without sealing `active_seg`.
                commit_dual_tail_meta_unix(
                    lane_fd,
                    lane_label,
                    &effective_old_meta,
                    expected_uid,
                    observer,
                )?;
                Ok(Phase2ConvergedState {
                    meta: effective_old_meta,
                    active_seg_exists: true,
                    recovered_frames: old_recovered_frames,
                    recovered_bytes: old_recovered_bytes,
                    mirrored_replica: true,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed,
                    quarantined_artifacts,
                    lane_dir_synced: true,
                })
            }
        }
    }

    /// Inode-pinned Unix unlink + `fsync_dir(lane_fd)` for a verified 0-byte
    /// Case D empty orphan successor segment (`ADR-RB-05` #3 Case D, `RB-08f` AC1–AC2).
    fn unlink_empty_orphan_at_fd_unix(
        lane_fd: &OwnedFd,
        lane: CaptureQueueLane,
        orphan_seg_id: u32,
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<()> {
        let seg_name = segment_log_file_name(orphan_seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open empty orphan segment")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue empty orphan segment"));
            }
            return Err(StorageBackendError::io("open empty orphan segment", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here and kept open until
        // after inode-verified `unlinkat`.
        let orphan_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&orphan_fd, "stat empty orphan segment")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue empty orphan segment is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(
                "capture queue empty orphan segment",
            ));
        }
        if st.st_size != 0 {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "expected 0-byte empty orphan segment became non-empty before unlink"
                    .to_string(),
            });
        }
        let expected_dev = st.st_dev;
        let expected_ino = st.st_ino;

        let mut cur_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let stat_rc = unsafe {
            libc::fstatat(
                lane_fd.as_raw_fd(),
                cname.as_ptr(),
                cur_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if stat_rc != 0 {
            return Err(StorageBackendError::io(
                "verify empty orphan segment inode before unlink",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let cur_st = unsafe { cur_st.assume_init() };
        if cur_st.st_dev != expected_dev || cur_st.st_ino != expected_ino {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "empty orphan segment inode changed before unlink".to_string(),
            });
        }

        let lane_label = lane_dir_name(lane);
        observer.record_orphan_unlink_start(lane_label, orphan_seg_id);
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let unlink_rc = unsafe { libc::unlinkat(lane_fd.as_raw_fd(), cname.as_ptr(), 0) };
        if unlink_rc != 0 {
            return Err(StorageBackendError::io(
                "unlink empty capture queue orphan segment",
                io::Error::last_os_error(),
            ));
        }
        drop(orphan_fd);
        observer.record_orphan_unlink_complete(lane_label, orphan_seg_id);

        observer.maybe_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink)?;
        fsync_fd(
            lane_fd,
            "fsync capture queue lane directory after unlinking empty orphan",
        )?;
        observer.record_fsync_lane_after_orphan_unlink(lane_label, orphan_seg_id);
        Ok(())
    }

    fn apply_mirror_sync_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        meta: &LaneTailMeta,
        mirror_sync: MirrorSync,
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<bool> {
        match mirror_sync {
            MirrorSync::None => Ok(false),
            MirrorSync::SyncBackup => {
                let encoded = meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                commit_single_meta_replica_unix(
                    lane_fd,
                    lane_label,
                    QUEUE_TAIL_META_BAK_TMP_FILE,
                    QUEUE_TAIL_META_BAK_FILE,
                    &encoded,
                    expected_uid,
                    observer,
                )?;
                Ok(true)
            }
            MirrorSync::SyncPrimary => {
                let encoded = meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                commit_single_meta_replica_unix(
                    lane_fd,
                    lane_label,
                    QUEUE_TAIL_META_TMP_FILE,
                    QUEUE_TAIL_META_FILE,
                    &encoded,
                    expected_uid,
                    observer,
                )?;
                Ok(true)
            }
        }
    }

    /// Shared locked Unix segment-file creation primitive (`ADR-RB-05` #3 Step ③,
    /// `RB-08g`): exclusively creates `events-<next_seg:08>.log` (`O_CREAT |
    /// O_EXCL | O_NOFOLLOW`), enforces exact `0600` permissions via `fchmod`,
    /// verifies `S_IFREG`/UID/`0600`/`st_size == 0`, and immediately calls
    /// `fsync_fd(lane_fd)`.
    pub(super) fn create_segment_file_at_fd_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        next_seg: u32,
        expected_uid: u32,
        _append_lock: &AppendLockGuard,
        observer: &mut SegmentObserver,
    ) -> StorageResult<File> {
        let seg_name = segment_log_file_name(next_seg);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "create capture queue segment")?;
        let flags =
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe {
            libc::openat(
                lane_fd.as_raw_fd(),
                cname.as_ptr(),
                flags,
                QUEUE_FILE_MODE as libc::c_uint,
            )
        };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            return Err(StorageBackendError::io("create capture queue segment", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut unlink_guard = TempUnlinkGuard::new(lane_fd, cname);

        let st = fstat_fd(&fd, "stat newly created capture queue segment")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "newly created capture queue segment is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue segment file"));
        }
        fchmod_fd(
            &fd,
            QUEUE_FILE_MODE,
            "enforce 0600 mode on newly created capture queue segment",
        )?;
        let verified = fstat_fd(&fd, "verify newly created capture queue segment mode")?;
        if unix_perm_bits(verified.st_mode) != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue segment permissions failed to settle at 0600".to_string(),
            });
        }
        let actual_len = u64::try_from(verified.st_size).map_err(|_| {
            StorageBackendError::CorruptCommittedFrame {
                detail: "newly created capture queue segment has negative size".to_string(),
            }
        })?;
        if actual_len != 0 {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "newly created capture queue segment is non-empty".to_string(),
            });
        }

        unlink_guard.disarm();
        observer.record_create_segment(lane_label, next_seg);
        fsync_fd(
            lane_fd,
            "fsync capture queue lane directory after creating segment",
        )?;
        observer.record_fsync_lane_after_create_segment(lane_label, next_seg);
        Ok(File::from(fd))
    }

    fn open_existing_active_segment_unix(
        lane_fd: &OwnedFd,
        seg_id: u32,
        expected_len: u64,
        expected_uid: u32,
    ) -> StorageResult<File> {
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open active capture queue segment")?;
        let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            return Err(StorageBackendError::io(
                "open active capture queue segment",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat active capture queue segment")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "active capture queue segment is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture queue segment file"));
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            fchmod_fd(
                &fd,
                QUEUE_FILE_MODE,
                "enforce 0600 mode on capture queue segment",
            )?;
            let verified = fstat_fd(&fd, "verify capture queue segment mode")?;
            if unix_perm_bits(verified.st_mode) != QUEUE_FILE_MODE {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture queue segment permissions failed to settle at 0600"
                        .to_string(),
                });
            }
        }
        let actual_len =
            u64::try_from(st.st_size).map_err(|_| StorageBackendError::CorruptCommittedFrame {
                detail: "active capture queue segment has negative size".to_string(),
            })?;
        if actual_len != expected_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail:
                    "active capture queue segment length diverged from committed_len before append"
                        .to_string(),
            });
        }
        Ok(File::from(fd))
    }

    fn open_quarantine_dir_fd(
        root_fd: &OwnedFd,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<OwnedFd> {
        let (q_fd, q_st) = openat_dir_nofollow(
            root_fd,
            OsStr::new(QUEUE_QUARANTINE_DIR),
            owners.quarantine_uid,
            "capture queue quarantine directory",
        )?;
        verify_dir_mode_0700(&q_st, "capture queue quarantine directory")?;
        Ok(q_fd)
    }

    /// Scan `quarantine_fd` strictly for lane-scoped temporary files
    /// `quarantine/<lane>-*.tmp` and clean ONLY those owned by `lane`, NEVER
    /// touching the opposite lane's `.tmp` files, unowned `.tmp`, or `.bin`
    /// artifacts (`ADR-RB-05` #3, `RB-08t` AC6). Must ONLY be called in Phase 2
    /// after Phase 1 read-only classification confirms `lane` is valid/recoverable.
    fn clean_lane_quarantine_tmp_files_unix(
        quarantine_fd: &OwnedFd,
        lane: CaptureQueueLane,
        expected_uid: u32,
    ) -> StorageResult<usize> {
        // SAFETY: `quarantine_fd` is a valid open directory descriptor.
        let dup_raw = unsafe { libc::fcntl(quarantine_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_raw < 0 {
            return Err(StorageBackendError::io(
                "duplicate capture queue quarantine descriptor",
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
                "open capture queue quarantine directory stream",
                err,
            ));
        }
        let guard = DirStreamGuard { ptr: dir_ptr };
        // SAFETY: `guard.ptr` is a valid open `DIR*`; rewind because
        // `F_DUPFD_CLOEXEC` shares the open file description's directory offset.
        unsafe {
            libc::rewinddir(guard.ptr);
        }
        let mut stale_names = Vec::new();
        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is a valid open `DIR*`.
            let entry = unsafe { libc::readdir(guard.ptr) };
            if entry.is_null() {
                let err = io::Error::last_os_error();
                if err.raw_os_error().unwrap_or(0) != 0 {
                    return Err(StorageBackendError::io(
                        "read capture queue quarantine directory entry",
                        err,
                    ));
                }
                break;
            }
            // SAFETY: `entry` is non-null and `d_name` is NUL-terminated.
            let name_bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if is_lane_quarantine_temp_name(lane, name_bytes) {
                stale_names.push(OsStr::from_bytes(name_bytes).to_os_string());
            }
        }
        drop(guard);
        clean_stale_tmp_files_unix(quarantine_fd, &stale_names, expected_uid)
    }

    /// Descriptor-pinned Unix implementation of `publish_quarantine_durable`
    /// (`ADR-RB-05` #3, `RB-08t` AC1–AC3, AC6–AC8):
    /// serialized under held `append.lock` (`_append_lock`), executes
    /// `openat(<id>.tmp, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600)`
    /// -> `fchmod(0600)` + verify -> bounded streaming `write_fn` ->
    /// `tmp_file.sync_data()` -> verify `(st_dev, st_ino)` pre-rename ->
    /// `renameat(<id>.tmp -> <id>.bin)` while `tmp_file` stays open ->
    /// verify `(st_dev, st_ino)` post-rename -> `fsync_dir(quarantine_fd)`.
    fn publish_quarantine_durable_writer_at_fd<F>(
        quarantine_fd: &OwnedFd,
        quarantine_dir: &Path,
        id: &str,
        expected_uid: u32,
        _append_lock: &AppendLockGuard,
        observer: &mut SegmentObserver,
        write_fn: F,
    ) -> StorageResult<QuarantinePublishReport>
    where
        F: FnOnce(&mut File) -> StorageResult<(u64, u32)>,
    {
        validate_quarantine_artifact_id(id)?;
        let tmp_name = format!("{id}.tmp");
        let bin_name = format!("{id}.bin");
        let ctmp = osstr_to_cstring(OsStr::new(&tmp_name), "create quarantine temp file")?;
        let cbin = osstr_to_cstring(OsStr::new(&bin_name), "publish quarantine bin file")?;

        // Verify any pre-existing `<id>.bin` (idempotent replay) is an owned
        // regular file and never a symlink/directory.
        let mut bin_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `quarantine_fd` is valid and `cbin` is NUL-terminated.
        let bin_rc = unsafe {
            libc::fstatat(
                quarantine_fd.as_raw_fd(),
                cbin.as_ptr(),
                bin_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if bin_rc == 0 {
            // SAFETY: `fstatat` succeeded.
            let bin_st = unsafe { bin_st.assume_init() };
            if (bin_st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue quarantine artifact"));
            }
            if (bin_st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture queue quarantine target is not a regular file".to_string(),
                });
            }
            if bin_st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err(
                    "capture queue quarantine artifact",
                ));
            }
        } else {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "inspect capture queue quarantine artifact",
                    err,
                ));
            }
        }

        // Open `<id>.tmp` exclusively with `0600` / `O_NOFOLLOW`. Because
        // `append.lock` is held across all publishers and lane recovery, any
        // pre-existing `<id>.tmp` is guaranteed stale from a prior crashed
        // publisher; verify it is an owned regular file before unlinking.
        let create_flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let tmp_fd = {
            let mut created = None;
            for attempt in 0..2 {
                // SAFETY: `quarantine_fd` is valid and `ctmp` is NUL-terminated.
                let raw_fd = unsafe {
                    libc::openat(
                        quarantine_fd.as_raw_fd(),
                        ctmp.as_ptr(),
                        create_flags,
                        QUEUE_FILE_MODE as libc::c_uint,
                    )
                };
                if raw_fd >= 0 {
                    // SAFETY: `raw_fd >= 0` is exclusively owned here.
                    created = Some(unsafe { OwnedFd::from_raw_fd(raw_fd) });
                    break;
                }
                let err = io::Error::last_os_error();
                if is_symlink_errno_at(&err, quarantine_fd, &ctmp) {
                    return Err(map_queue_symlink_err("capture queue quarantine temp"));
                }
                if err.kind() == io::ErrorKind::AlreadyExists && attempt == 0 {
                    let mut stale_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
                    // SAFETY: `quarantine_fd` is valid and `ctmp` is NUL-terminated.
                    let st_rc = unsafe {
                        libc::fstatat(
                            quarantine_fd.as_raw_fd(),
                            ctmp.as_ptr(),
                            stale_st.as_mut_ptr(),
                            libc::AT_SYMLINK_NOFOLLOW,
                        )
                    };
                    if st_rc != 0 {
                        let st_err = io::Error::last_os_error();
                        if st_err.kind() == io::ErrorKind::NotFound {
                            continue;
                        }
                        return Err(StorageBackendError::io(
                            "inspect stale quarantine temp file",
                            st_err,
                        ));
                    }
                    // SAFETY: `fstatat` succeeded.
                    let stale_st = unsafe { stale_st.assume_init() };
                    if (stale_st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                        return Err(map_queue_symlink_err("capture queue quarantine temp"));
                    }
                    if (stale_st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "capture queue quarantine temp entry is not a regular file"
                                .to_string(),
                        });
                    }
                    if stale_st.st_uid != expected_uid {
                        return Err(map_queue_owner_mismatch_err(
                            "capture queue quarantine temp",
                        ));
                    }
                    // SAFETY: `quarantine_fd` is valid and `ctmp` is NUL-terminated.
                    let unlink_rc =
                        unsafe { libc::unlinkat(quarantine_fd.as_raw_fd(), ctmp.as_ptr(), 0) };
                    if unlink_rc != 0 {
                        let uerr = io::Error::last_os_error();
                        if uerr.kind() != io::ErrorKind::NotFound {
                            return Err(StorageBackendError::io(
                                "unlink stale quarantine temp file",
                                uerr,
                            ));
                        }
                    }
                    continue;
                }
                return Err(StorageBackendError::io("create quarantine temp file", err));
            }
            created.ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "failed to create quarantine temp file after stale cleanup".to_string(),
            })?
        };
        let mut unlink_guard = TempUnlinkGuard::new(quarantine_fd, ctmp.clone());

        fchmod_fd(
            &tmp_fd,
            QUEUE_FILE_MODE,
            "enforce 0600 mode on quarantine temp file",
        )?;
        let st = fstat_fd(&tmp_fd, "verify quarantine temp file mode")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue quarantine temp is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(
                "capture queue quarantine temp",
            ));
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue quarantine temp permissions failed to settle at 0600"
                    .to_string(),
            });
        }
        let tmp_dev = st.st_dev;
        let tmp_ino = st.st_ino;

        let mut tmp_file = File::from(tmp_fd);
        let (quarantined_bytes, crc32c) = write_fn(&mut tmp_file)?;

        // Step 1/2: `tmp_file.sync_data()` strictly BEFORE `rename(.tmp -> .bin)`.
        observer.record_quarantine_tmp_sync_start(id);
        tmp_file
            .sync_data()
            .map_err(|e| StorageBackendError::io("sync quarantine temp data", e))?;
        observer.record_quarantine_tmp_sync_complete(id);

        if let Err(err) = observer.maybe_crash(SegmentCrashPoint::BeforeQuarantineRename) {
            // Preserve `<id>.tmp` on disk to faithfully simulate `SIGKILL` before rename.
            unlink_guard.disarm();
            drop(tmp_file);
            return Err(err);
        }

        // Verify `<id>.tmp` still resolves to the exact synced `(st_dev, st_ino)`
        // before calling `renameat` (defense-in-depth against external tampering).
        let mut pre_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `quarantine_fd` is valid and `ctmp` is NUL-terminated.
        let pre_rc = unsafe {
            libc::fstatat(
                quarantine_fd.as_raw_fd(),
                ctmp.as_ptr(),
                pre_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if pre_rc != 0 {
            return Err(StorageBackendError::io(
                "verify quarantine temp inode before rename",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let pre_st = unsafe { pre_st.assume_init() };
        if pre_st.st_dev != tmp_dev || pre_st.st_ino != tmp_ino {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue quarantine temp inode changed before rename".to_string(),
            });
        }

        // Step 3: Atomic `renameat(.tmp -> .bin)` while keeping `tmp_file` open.
        observer.record_quarantine_rename_start(id);
        // SAFETY: `quarantine_fd` is valid and both CStrings are NUL-terminated.
        let rename_rc = unsafe {
            libc::renameat(
                quarantine_fd.as_raw_fd(),
                ctmp.as_ptr(),
                quarantine_fd.as_raw_fd(),
                cbin.as_ptr(),
            )
        };
        if rename_rc != 0 {
            return Err(StorageBackendError::io(
                "atomically publish quarantine artifact",
                io::Error::last_os_error(),
            ));
        }
        unlink_guard.disarm();

        // Confirm `<id>.bin` now refers to the exact synced `(st_dev, st_ino)`
        // while `tmp_file` keeps that inode pinned.
        let mut post_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `quarantine_fd` is valid and `cbin` is NUL-terminated.
        let post_rc = unsafe {
            libc::fstatat(
                quarantine_fd.as_raw_fd(),
                cbin.as_ptr(),
                post_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if post_rc != 0 {
            return Err(StorageBackendError::io(
                "verify quarantine bin inode after rename",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let post_st = unsafe { post_st.assume_init() };
        if post_st.st_dev != tmp_dev || post_st.st_ino != tmp_ino {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue quarantine bin inode mismatch after rename".to_string(),
            });
        }
        drop(tmp_file);
        observer.record_quarantine_rename_complete(id);

        // Step 4: `fsync_dir(quarantine_dir)` strictly AFTER `rename` and BEFORE
        // any destructive `truncate` or `unlink`.
        observer.record_quarantine_fsync_dir_start(id);
        fsync_fd(
            quarantine_fd,
            "fsync capture queue quarantine directory after publish",
        )?;
        observer.record_quarantine_fsync_dir_complete(id);

        observer.maybe_crash(SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive)?;

        Ok(QuarantinePublishReport {
            id: id.to_string(),
            bin_path: quarantine_dir.join(bin_name),
            quarantined_bytes,
            crc32c,
            diagnostic_code: QUARANTINE_DIAGNOSTIC_CODE,
        })
    }

    pub(super) fn publish_quarantine_durable_at_fd(
        quarantine_fd: &OwnedFd,
        quarantine_dir: &Path,
        id: &str,
        bytes: &[u8],
        expected_uid: u32,
        append_lock: &AppendLockGuard,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        let len_u64 =
            u64::try_from(bytes.len()).map_err(|_| StorageBackendError::BackendInternal {
                detail: "quarantine payload length exceeds u64".to_string(),
            })?;
        let crc = crc32c_checksum(bytes);
        publish_quarantine_durable_writer_at_fd(
            quarantine_fd,
            quarantine_dir,
            id,
            expected_uid,
            append_lock,
            observer,
            |tmp_file| {
                tmp_file
                    .write_all(bytes)
                    .map_err(|e| StorageBackendError::io("write quarantine temp bytes", e))?;
                Ok((len_u64, crc))
            },
        )
    }

    /// Pin `queue_root = quarantine_dir.parent()` (`0700`, `O_NOFOLLOW`),
    /// acquire `<queue_root>/append.lock` strictly (`0600`), and open
    /// `quarantine_fd` (`0700`, `O_NOFOLLOW`) relative to `root_fd`.
    fn acquire_quarantine_root_lock_unix(
        quarantine_dir: &Path,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<(OwnedFd, OwnedFd, AppendLockGuard)> {
        let queue_root = quarantine_dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "capture queue quarantine directory has no parent queue root".to_string(),
            })?;
        let q_name =
            quarantine_dir
                .file_name()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "capture queue quarantine directory has no basename".to_string(),
                })?;
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        let deadline = lock_deadline_for_timeout(DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT);
        let append_lock = match try_acquire_existing_strict_append_lock_unix(
            &root_fd,
            owners.root_uid,
            deadline,
        )? {
            Some(lock) => lock,
            None => create_or_acquire_strict_append_lock_unix(&root_fd, owners.root_uid, deadline)?,
        };
        let (quarantine_fd, q_st) = openat_dir_nofollow(
            &root_fd,
            q_name,
            owners.quarantine_uid,
            "capture queue quarantine directory",
        )?;
        verify_dir_mode_0700(&q_st, "capture queue quarantine directory")?;
        Ok((root_fd, quarantine_fd, append_lock))
    }

    pub(super) struct UnixQuarantineDirs<'a> {
        pub(super) quarantine_fd: &'a OwnedFd,
        pub(super) quarantine_dir: &'a Path,
        pub(super) lane_fd: &'a OwnedFd,
        pub(super) owners: UnixExpectedQueueOwners,
        pub(super) append_lock: &'a AppendLockGuard,
    }

    pub(super) fn quarantine_and_truncate_segment_at_fds(
        dirs: &UnixQuarantineDirs<'_>,
        lane: CaptureQueueLane,
        seg_id: u32,
        valid_end: u64,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        let lane_uid = lane_expected_uid(dirs.owners, lane);
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(
            OsStr::new(&seg_name),
            "open segment for quarantine truncate",
        )?;
        let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `dirs.lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(dirs.lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, dirs.lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            return Err(StorageBackendError::io(
                "open segment for quarantine truncate",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let seg_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&seg_fd, "stat segment for quarantine truncate")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment path is not a regular file".to_string(),
            });
        }
        if st.st_uid != lane_uid {
            return Err(map_queue_owner_mismatch_err("capture queue segment file"));
        }
        let actual_len =
            u64::try_from(st.st_size).map_err(|_| StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment has negative size".to_string(),
            })?;
        if valid_end > actual_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "truncate valid_end exceeds segment actual length".to_string(),
            });
        }
        let tail_len = actual_len - valid_end;
        if tail_len == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "quarantine_and_truncate_segment requires non-empty damaged tail"
                    .to_string(),
            });
        }

        let mut seg_file = File::from(seg_fd);
        // Stream-hash with a fixed 64 KiB stack buffer (zero unbounded heap allocation).
        let expected_crc = stream_crc32c_file_range(&mut seg_file, valid_end, tail_len)?;
        let id = quarantine_artifact_id_from_crc(lane, seg_id, valid_end, expected_crc);

        // Mandatory durable quarantine barrier BEFORE destructive `set_len`.
        let report = publish_quarantine_durable_writer_at_fd(
            dirs.quarantine_fd,
            dirs.quarantine_dir,
            &id,
            dirs.owners.quarantine_uid,
            dirs.append_lock,
            observer,
            |tmp_file| {
                let copied_crc =
                    stream_copy_file_range_with_crc(&mut seg_file, valid_end, tail_len, tmp_file)?;
                if copied_crc != expected_crc {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "capture queue segment tail changed while streaming to quarantine"
                            .to_string(),
                    });
                }
                Ok((tail_len, copied_crc))
            },
        )?;

        let lane_label = lane_dir_name(lane);
        observer.record_segment_truncate_start(lane_label, seg_id, valid_end);
        seg_file
            .set_len(valid_end)
            .map_err(|e| StorageBackendError::io("truncate capture queue segment", e))?;
        observer.record_segment_truncate_complete(lane_label, seg_id, valid_end);
        seg_file
            .sync_data()
            .map_err(|e| StorageBackendError::io("sync truncated capture queue segment", e))?;
        observer.record_segment_sync_data(lane_label, seg_id);
        drop(seg_file);

        Ok(report)
    }

    pub(super) fn quarantine_and_unlink_orphan_at_fds(
        dirs: &UnixQuarantineDirs<'_>,
        lane: CaptureQueueLane,
        orphan_seg_id: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        let lane_uid = lane_expected_uid(dirs.owners, lane);
        let seg_name = segment_log_file_name(orphan_seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open orphan segment for quarantine")?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `dirs.lane_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(dirs.lane_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, dirs.lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue orphan segment file"));
            }
            return Err(StorageBackendError::io(
                "open orphan segment for quarantine",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here and stays open until
        // after inode-pinned `unlinkat`.
        let orphan_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&orphan_fd, "stat orphan segment for quarantine")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue orphan segment path is not a regular file".to_string(),
            });
        }
        if st.st_uid != lane_uid {
            return Err(map_queue_owner_mismatch_err(
                "capture queue orphan segment file",
            ));
        }
        let expected_dev = st.st_dev;
        let expected_ino = st.st_ino;
        let actual_len =
            u64::try_from(st.st_size).map_err(|_| StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue orphan segment has negative size".to_string(),
            })?;
        if actual_len == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "quarantine_and_unlink_orphan requires non-empty orphan segment"
                    .to_string(),
            });
        }

        let mut orphan_file = File::from(orphan_fd);
        // Stream-hash with a fixed 64 KiB stack buffer (zero unbounded heap allocation).
        let expected_crc = stream_crc32c_file_range(&mut orphan_file, 0, actual_len)?;
        let id = quarantine_artifact_id_from_crc(lane, orphan_seg_id, 0, expected_crc);

        // Mandatory durable quarantine barrier BEFORE destructive `unlinkat`.
        let report = publish_quarantine_durable_writer_at_fd(
            dirs.quarantine_fd,
            dirs.quarantine_dir,
            &id,
            dirs.owners.quarantine_uid,
            dirs.append_lock,
            observer,
            |tmp_file| {
                let copied_crc =
                    stream_copy_file_range_with_crc(&mut orphan_file, 0, actual_len, tmp_file)?;
                if copied_crc != expected_crc {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail:
                            "capture queue orphan segment changed while streaming to quarantine"
                                .to_string(),
                    });
                }
                Ok((actual_len, copied_crc))
            },
        )?;

        // Verify directory entry still points to the exact pinned `(st_dev, st_ino)`.
        let mut cur_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `dirs.lane_fd` is valid and `cname` is NUL-terminated.
        let stat_rc = unsafe {
            libc::fstatat(
                dirs.lane_fd.as_raw_fd(),
                cname.as_ptr(),
                cur_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if stat_rc != 0 {
            return Err(StorageBackendError::io(
                "verify orphan segment inode before unlink",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let cur_st = unsafe { cur_st.assume_init() };
        if cur_st.st_dev != expected_dev || cur_st.st_ino != expected_ino {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "orphan segment inode changed before unlink".to_string(),
            });
        }

        let lane_label = lane_dir_name(lane);
        observer.record_orphan_unlink_start(lane_label, orphan_seg_id);
        // SAFETY: `dirs.lane_fd` is valid and `cname` is NUL-terminated.
        let unlink_rc = unsafe { libc::unlinkat(dirs.lane_fd.as_raw_fd(), cname.as_ptr(), 0) };
        if unlink_rc != 0 {
            return Err(StorageBackendError::io(
                "unlink quarantined capture queue orphan segment",
                io::Error::last_os_error(),
            ));
        }
        drop(orphan_file);
        observer.record_orphan_unlink_complete(lane_label, orphan_seg_id);

        observer.maybe_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink)?;
        fsync_fd(
            dirs.lane_fd,
            "fsync capture queue lane directory after unlinking quarantined orphan",
        )?;
        observer.record_fsync_lane_after_orphan_unlink(lane_label, orphan_seg_id);

        Ok(report)
    }

    pub(super) fn publish_quarantine_durable_unix(
        quarantine_dir: &Path,
        id: &str,
        bytes: &[u8],
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        let (_root_fd, quarantine_fd, append_lock) =
            acquire_quarantine_root_lock_unix(quarantine_dir, owners)?;
        publish_quarantine_durable_at_fd(
            &quarantine_fd,
            quarantine_dir,
            id,
            bytes,
            owners.quarantine_uid,
            &append_lock,
            observer,
        )
    }

    pub(super) fn quarantine_and_truncate_segment_unix(
        quarantine_dir: &Path,
        lane_dir: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        valid_end: u64,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        if quarantine_dir.parent() != lane_dir.parent() {
            return Err(StorageBackendError::BackendInternal {
                detail: "quarantine and lane directories must share the same capture queue root"
                    .to_string(),
            });
        }
        let (root_fd, quarantine_fd, append_lock) =
            acquire_quarantine_root_lock_unix(quarantine_dir, owners)?;
        let lane_name =
            lane_dir
                .file_name()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "capture queue lane directory has no basename".to_string(),
                })?;
        let lane_uid = lane_expected_uid(owners, lane);
        let (lane_fd, lane_st) = openat_dir_nofollow(
            &root_fd,
            lane_name,
            lane_uid,
            "capture queue lane directory",
        )?;
        verify_dir_mode_0700(&lane_st, "capture queue lane directory")?;
        let dirs = UnixQuarantineDirs {
            quarantine_fd: &quarantine_fd,
            quarantine_dir,
            lane_fd: &lane_fd,
            owners,
            append_lock: &append_lock,
        };
        quarantine_and_truncate_segment_at_fds(&dirs, lane, seg_id, valid_end, observer)
    }

    pub(super) fn quarantine_and_unlink_orphan_unix(
        quarantine_dir: &Path,
        lane_dir: &Path,
        lane: CaptureQueueLane,
        orphan_seg_id: u32,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        if quarantine_dir.parent() != lane_dir.parent() {
            return Err(StorageBackendError::BackendInternal {
                detail: "quarantine and lane directories must share the same capture queue root"
                    .to_string(),
            });
        }
        let (root_fd, quarantine_fd, append_lock) =
            acquire_quarantine_root_lock_unix(quarantine_dir, owners)?;
        let lane_name =
            lane_dir
                .file_name()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "capture queue lane directory has no basename".to_string(),
                })?;
        let lane_uid = lane_expected_uid(owners, lane);
        let (lane_fd, lane_st) = openat_dir_nofollow(
            &root_fd,
            lane_name,
            lane_uid,
            "capture queue lane directory",
        )?;
        verify_dir_mode_0700(&lane_st, "capture queue lane directory")?;
        let dirs = UnixQuarantineDirs {
            quarantine_fd: &quarantine_fd,
            quarantine_dir,
            lane_fd: &lane_fd,
            owners,
            append_lock: &append_lock,
        };
        quarantine_and_unlink_orphan_at_fds(&dirs, lane, orphan_seg_id, observer)
    }

    /// Resolve the sibling lane's authoritative `min_live_seg` read-only so
    /// capacity accounting skips already-reclaimed orphan segments while still
    /// conservatively counting all `events-*.log` files (`min_live_seg = 0`) if
    /// sibling metadata is absent or corrupt. Security violations (symlinks,
    /// non-regular metadata, UID mismatch) always fail closed.
    fn resolve_lane_min_live_seg_readonly_unix(
        lane_fd: &OwnedFd,
        expected_uid: u32,
    ) -> StorageResult<u32> {
        let primary = read_meta_replica_nofollow(
            lane_fd,
            QUEUE_TAIL_META_FILE,
            expected_uid,
            "capture queue tail.meta",
        )?;
        let backup = read_meta_replica_nofollow(
            lane_fd,
            QUEUE_TAIL_META_BAK_FILE,
            expected_uid,
            "capture queue tail.meta.bak",
        )?;
        match resolve_dual_replicas(primary, backup)? {
            ResolvedReplicas::Authoritative { meta, .. } => Ok(meta.min_live_seg),
            ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => Ok(0),
        }
    }

    /// Measure physical unconsumed segment bytes (`events-*.log` with
    /// `seg_id >= min_live_seg`, plus any malformed `events-*.log` entries
    /// conservatively) in `lane_fd` using `fstatat(AT_SYMLINK_NOFOLLOW)` with
    /// symlink, regular-file, and effective-UID verification (`RB-08b`).
    pub(super) fn measure_lane_unconsumed_bytes_unix(
        lane_fd: &OwnedFd,
        min_live_seg: u32,
        expected_uid: u32,
    ) -> StorageResult<u64> {
        // SAFETY: `lane_fd` is a valid open directory descriptor.
        let dup_raw = unsafe { libc::fcntl(lane_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_raw < 0 {
            return Err(StorageBackendError::io(
                "duplicate capture queue lane descriptor for occupancy scan",
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
                "open capture queue lane directory stream for occupancy scan",
                err,
            ));
        }
        let guard = DirStreamGuard { ptr: dir_ptr };
        // SAFETY: `guard.ptr` is a valid open `DIR*`; rewind because
        // `F_DUPFD_CLOEXEC` shares the open file description's directory offset.
        unsafe {
            libc::rewinddir(guard.ptr);
        }
        let mut total = 0u64;

        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is a valid open `DIR*`.
            let entry = unsafe { libc::readdir(guard.ptr) };
            if entry.is_null() {
                let err = io::Error::last_os_error();
                if err.raw_os_error().unwrap_or(0) != 0 {
                    return Err(StorageBackendError::io(
                        "read capture queue lane entry for occupancy scan",
                        err,
                    ));
                }
                break;
            }
            // SAFETY: `entry` is non-null and `d_name` is NUL-terminated.
            let cname = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name_bytes = cname.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            if !matches_segment_log_glob_bytes(name_bytes) {
                continue;
            }
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    lane_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(StorageBackendError::io(
                    "stat capture queue segment for occupancy",
                    io::Error::last_os_error(),
                ));
            }
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            let ftype = st.st_mode & libc::S_IFMT;
            if ftype == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            if ftype != libc::S_IFREG {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "capture queue segment path is not a regular file".to_string(),
                });
            }
            if st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err("capture queue segment file"));
            }
            let size = u64::try_from(st.st_size).map_err(|_| {
                StorageBackendError::CorruptCommittedFrame {
                    detail: "capture queue segment has negative size".to_string(),
                }
            })?;
            // Exclude already-reclaimed segments ONLY after verifying symlink,
            // regular-file, and owner invariants so a hostile entry planted at
            // a reclaimed segment name can never bypass fail-closed checks.
            if let Some(seg_id) = parse_segment_log_file_name(name_bytes)
                && seg_id < min_live_seg
            {
                continue;
            }
            total = total
                .checked_add(size)
                .ok_or_else(|| StorageBackendError::QueueFull {
                    detail: "capture queue lane occupancy overflowed u64".to_string(),
                })?;
        }
        Ok(total)
    }

    /// Measure physical bytes occupied by all regular files inside
    /// `quarantine/` (published `.bin` artifacts plus any opposite-lane `.tmp`
    /// files) via `fstatat(AT_SYMLINK_NOFOLLOW)` with symlink, regular-file,
    /// and effective-UID verification (`RB-08b`).
    pub(super) fn measure_quarantine_bytes_unix(
        quarantine_fd: &OwnedFd,
        expected_uid: u32,
    ) -> StorageResult<u64> {
        // SAFETY: `quarantine_fd` is a valid open directory descriptor.
        let dup_raw = unsafe { libc::fcntl(quarantine_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_raw < 0 {
            return Err(StorageBackendError::io(
                "duplicate capture queue quarantine descriptor for occupancy scan",
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
                "open capture queue quarantine directory stream for occupancy scan",
                err,
            ));
        }
        let guard = DirStreamGuard { ptr: dir_ptr };
        // SAFETY: `guard.ptr` is a valid open `DIR*`; rewind because
        // `F_DUPFD_CLOEXEC` shares the open file description's directory offset.
        unsafe {
            libc::rewinddir(guard.ptr);
        }
        let mut total = 0u64;

        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is a valid open `DIR*`.
            let entry = unsafe { libc::readdir(guard.ptr) };
            if entry.is_null() {
                let err = io::Error::last_os_error();
                if err.raw_os_error().unwrap_or(0) != 0 {
                    return Err(StorageBackendError::io(
                        "read capture queue quarantine entry for occupancy scan",
                        err,
                    ));
                }
                break;
            }
            // SAFETY: `entry` is non-null and `d_name` is NUL-terminated.
            let cname = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name_bytes = cname.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `quarantine_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    quarantine_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(StorageBackendError::io(
                    "stat capture queue quarantine entry for occupancy",
                    io::Error::last_os_error(),
                ));
            }
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            let ftype = st.st_mode & libc::S_IFMT;
            if ftype == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue quarantine entry"));
            }
            if ftype != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture queue quarantine entry is not a regular file".to_string(),
                });
            }
            if st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err(
                    "capture queue quarantine entry",
                ));
            }
            let size =
                u64::try_from(st.st_size).map_err(|_| StorageBackendError::BackendInternal {
                    detail: "capture queue quarantine entry has negative size".to_string(),
                })?;
            total = total
                .checked_add(size)
                .ok_or_else(|| StorageBackendError::QueueFull {
                    detail: "capture queue quarantine occupancy overflowed u64".to_string(),
                })?;
        }
        Ok(total)
    }

    fn measure_cross_lane_occupancy_for_target_unix(
        root_fd: &OwnedFd,
        target_lane: CaptureQueueLane,
        target_lane_fd: &OwnedFd,
        target_min_live_seg: u32,
        quarantine_fd: &OwnedFd,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<CaptureQueueOccupancy> {
        let target_uid = lane_expected_uid(owners, target_lane);
        let target_bytes =
            measure_lane_unconsumed_bytes_unix(target_lane_fd, target_min_live_seg, target_uid)?;

        let other_lane = opposite_lane(target_lane);
        let other_lane_fd = open_lane_dir_fd(root_fd, other_lane, owners)?;
        let other_uid = lane_expected_uid(owners, other_lane);
        let other_min_live_seg =
            resolve_lane_min_live_seg_readonly_unix(&other_lane_fd, other_uid)?;
        let other_bytes =
            measure_lane_unconsumed_bytes_unix(&other_lane_fd, other_min_live_seg, other_uid)?;

        let quarantine_bytes = measure_quarantine_bytes_unix(quarantine_fd, owners.quarantine_uid)?;
        let (priority_bytes, normal_bytes) = match target_lane {
            CaptureQueueLane::Priority => (target_bytes, other_bytes),
            CaptureQueueLane::Normal => (other_bytes, target_bytes),
        };
        CaptureQueueOccupancy::from_parts(priority_bytes, normal_bytes, quarantine_bytes)
    }

    fn measure_occupancy_under_root_fd_unix(
        root_fd: &OwnedFd,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<CaptureQueueOccupancy> {
        let deadline = lock_deadline_for_timeout(DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT);
        let _lock =
            try_acquire_existing_strict_append_lock_unix(root_fd, owners.root_uid, deadline)?;
        let priority_fd = open_lane_dir_fd(root_fd, CaptureQueueLane::Priority, owners)?;
        let normal_fd = open_lane_dir_fd(root_fd, CaptureQueueLane::Normal, owners)?;
        let quarantine_fd = open_quarantine_dir_fd(root_fd, owners)?;

        let priority_min =
            resolve_lane_min_live_seg_readonly_unix(&priority_fd, owners.priority_uid)?;
        let normal_min = resolve_lane_min_live_seg_readonly_unix(&normal_fd, owners.normal_uid)?;

        let priority_bytes =
            measure_lane_unconsumed_bytes_unix(&priority_fd, priority_min, owners.priority_uid)?;
        let normal_bytes =
            measure_lane_unconsumed_bytes_unix(&normal_fd, normal_min, owners.normal_uid)?;
        let quarantine_bytes =
            measure_quarantine_bytes_unix(&quarantine_fd, owners.quarantine_uid)?;

        CaptureQueueOccupancy::from_parts(priority_bytes, normal_bytes, quarantine_bytes)
    }

    fn append_under_root_fd_unix(
        root_fd: &OwnedFd,
        quarantine_dir: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        options: QueueAppendOptions,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let lane_label = lane_dir_name(lane);
        let lane_uid = lane_expected_uid(owners, lane);
        let lane_fd = open_lane_dir_fd(root_fd, lane, owners)?;
        let quarantine_fd = open_quarantine_dir_fd(root_fd, owners)?;

        // Phase 1: acquire lock without pre-classification disk mutation and
        // execute strictly 100% read-only classification.
        let (append_lock, plan) = acquire_lock_and_phase1_unix(
            root_fd,
            &lane_fd,
            owners,
            lane_uid,
            options.effective_lock_timeout(),
            observer,
        )?;

        // Phase 2: clean lane-scoped quarantine `.tmp` (NEVER touching the
        // opposite lane's `.tmp`), clean lane `tail.meta*.tmp`, mirror lagging
        // replica, recover Case C / Case D.
        let _cleaned_q_tmp =
            clean_lane_quarantine_tmp_files_unix(&quarantine_fd, lane, owners.quarantine_uid)?;
        let phase2_ctx = UnixPhase2Context {
            lane,
            lane_fd: &lane_fd,
            quarantine_fd: &quarantine_fd,
            quarantine_dir,
            owners,
            append_lock: &append_lock,
        };
        let converged = phase2_execute_plan_unix(&phase2_ctx, plan, observer)?;

        // Step ② (a) (`ADR-RB-05` #3, `RB-08b`): pre-write cross-lane capacity
        // conservation guard AFTER Step ① recovery and strictly BEFORE creating
        // or writing any bytes for the incoming frame.
        let max_total_bytes = options.effective_max_total_bytes();
        let occupancy = measure_cross_lane_occupancy_for_target_unix(
            root_fd,
            lane,
            &lane_fd,
            converged.meta.min_live_seg,
            &quarantine_fd,
            owners,
        )?;
        observer.record_capacity_checked(occupancy, frame_buf.len(), max_total_bytes);
        let _ = enforce_queue_capacity_limit(occupancy, frame_buf.len(), max_total_bytes)?;

        // Step ② (b) (`ADR-RB-05` #3, `RB-08j`) + Step ③ (`RB-08g`): check
        // whether this frame requires 16 MiB rotation, enforce `sealed.len() <
        // MAX_SEALED_SEGMENTS` (`256`) BEFORE creating `S + 1` or modifying
        // any file for the incoming frame, then build metadata and create/open.
        let rotating = should_rotate_active_segment(
            converged.meta.committed_len,
            frame_buf.len(),
            CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES,
        )?;
        enforce_rotation_sealed_cap_limit(converged.meta.sealed.len(), rotating)?;
        let (seg_id, frame_offset, next_meta) =
            build_next_append_meta(converged.meta, frame_buf, rotating)?;
        let mut seg_file = if rotating || !converged.active_seg_exists {
            if frame_offset != 0 {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "cannot create active capture queue segment with non-zero offset"
                        .to_string(),
                });
            }
            create_segment_file_at_fd_unix(
                &lane_fd,
                lane_label,
                seg_id,
                lane_uid,
                &append_lock,
                observer,
            )?
        } else {
            open_existing_active_segment_unix(&lane_fd, seg_id, frame_offset, lane_uid)?
        };

        seg_file
            .seek(SeekFrom::Start(frame_offset))
            .map_err(|e| StorageBackendError::io("seek active capture queue segment", e))?;

        // AC2: Single contiguous `write_all` + `seg.sync_data()` inside `append.lock`.
        seg_file
            .write_all(frame_buf)
            .map_err(|e| StorageBackendError::io("write capture queue frame", e))?;
        observer.record_segment_write_all(lane_label, seg_id, frame_offset, frame_buf.len());
        seg_file
            .sync_data()
            .map_err(|e| StorageBackendError::io("sync capture queue segment data", e))?;
        observer.record_segment_sync_data(lane_label, seg_id);
        drop(seg_file);

        observer.maybe_crash(SegmentCrashPoint::AfterSegmentSyncData)?;

        // AC3 + AC4: Commit `tail.meta` and `tail.meta.bak` sequentially with
        // `sync_data -> rename -> fsync_dir(lane_dir)` BEFORE releasing `append.lock`.
        commit_dual_tail_meta_unix(&lane_fd, lane_label, &next_meta, lane_uid, observer)?;

        drop(append_lock);
        observer.record_release_lock();

        Ok(SegmentAppendOutcome {
            lane,
            seg_id,
            frame_offset,
            frame_len: frame_buf.len(),
            meta: next_meta,
            recovered_frames: converged.recovered_frames,
        })
    }

    fn recover_under_root_fd_unix(
        root_fd: &OwnedFd,
        quarantine_dir: &Path,
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        let lane_uid = lane_expected_uid(owners, lane);
        let lane_fd = open_lane_dir_fd(root_fd, lane, owners)?;
        let quarantine_fd = open_quarantine_dir_fd(root_fd, owners)?;

        let (append_lock, plan) = acquire_lock_and_phase1_unix(
            root_fd,
            &lane_fd,
            owners,
            lane_uid,
            DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT,
            observer,
        )?;
        let cleaned_quarantine_tmp_files =
            clean_lane_quarantine_tmp_files_unix(&quarantine_fd, lane, owners.quarantine_uid)?;
        let phase2_ctx = UnixPhase2Context {
            lane,
            lane_fd: &lane_fd,
            quarantine_fd: &quarantine_fd,
            quarantine_dir,
            owners,
            append_lock: &append_lock,
        };
        let converged = phase2_execute_plan_unix(&phase2_ctx, plan, observer)?;
        // Guarantee lane directory entry mutations from any prior interrupted
        // unlink (where `unlink` succeeded but `fsync_dir(lane_dir)` failed) are
        // durably persisted before `recover_lane` returns `Ok`.
        if !converged.lane_dir_synced {
            fsync_fd(
                &lane_fd,
                "fsync capture queue lane directory before completing recovery",
            )?;
            observer.record_fsync_lane_on_recovery(lane_dir_name(lane));
        }

        drop(append_lock);
        observer.record_release_lock();

        Ok(LaneRecoveryReport {
            lane,
            meta: converged.meta,
            recovered_frames: converged.recovered_frames,
            recovered_bytes: converged.recovered_bytes,
            mirrored_replica: converged.mirrored_replica,
            cleaned_tmp_files: converged.cleaned_tmp_files,
            cleaned_quarantine_tmp_files,
            reclaimed_orphans_removed: converged.reclaimed_orphans_removed,
            quarantined_artifacts: converged.quarantined_artifacts,
        })
    }

    pub(super) fn append_in_storage_dir_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        options: QueueAppendOptions,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        let quarantine_dir = storage_dir
            .join(PRIVATE_SUBDIR)
            .join(CAPTURE_QUEUE_SUBDIR)
            .join(QUEUE_QUARANTINE_DIR);
        append_under_root_fd_unix(
            &root_fd,
            &quarantine_dir,
            lane,
            frame_buf,
            options,
            owners,
            observer,
        )
    }

    pub(super) fn append_at_root_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        options: QueueAppendOptions,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
        append_under_root_fd_unix(
            &root_fd,
            &quarantine_dir,
            lane,
            frame_buf,
            options,
            owners,
            observer,
        )
    }

    pub(super) fn measure_occupancy_in_storage_dir_unix(
        storage_dir: &Path,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<CaptureQueueOccupancy> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        measure_occupancy_under_root_fd_unix(&root_fd, owners)
    }

    pub(super) fn measure_occupancy_at_root_unix(
        queue_root: &Path,
        owners: UnixExpectedQueueOwners,
    ) -> StorageResult<CaptureQueueOccupancy> {
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        measure_occupancy_under_root_fd_unix(&root_fd, owners)
    }

    pub(super) fn recover_in_storage_dir_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        let quarantine_dir = storage_dir
            .join(PRIVATE_SUBDIR)
            .join(CAPTURE_QUEUE_SUBDIR)
            .join(QUEUE_QUARANTINE_DIR);
        recover_under_root_fd_unix(&root_fd, &quarantine_dir, lane, owners, observer)
    }

    pub(super) fn recover_at_root_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
        recover_under_root_fd_unix(&root_fd, &quarantine_dir, lane, owners, observer)
    }

    /// Pre-verify that `cname` inside `lane_fd` exists on disk as a regular
    /// non-symlink file owned by `expected_uid` strictly BEFORE any Phase 2
    /// metadata mutation or `.tmp` cleanup.
    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    fn preverify_reclaim_target_unix(
        lane_fd: &OwnedFd,
        cname: &std::ffi::CStr,
        expected_uid: u32,
    ) -> StorageResult<()> {
        let mut pre_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let pre_rc = unsafe {
            libc::fstatat(
                lane_fd.as_raw_fd(),
                cname.as_ptr(),
                pre_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if pre_rc != 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "reclaimed capture queue sealed segment is missing on disk".to_string(),
                });
            }
            return Err(StorageBackendError::io(
                "stat reclaimed capture queue segment",
                err,
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let pre_st = unsafe { pre_st.assume_init() };
        if (pre_st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
            return Err(map_queue_symlink_err("reclaimed capture queue segment"));
        }
        if (pre_st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "reclaimed capture queue segment is not a regular file".to_string(),
            });
        }
        if pre_st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(
                "reclaimed capture queue segment",
            ));
        }
        Ok(())
    }

    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    fn reclaim_under_root_fd_unix(
        root_fd: &OwnedFd,
        quarantine_dir: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        highwater_seg: u32,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        let lane_label = lane_dir_name(lane);
        let lane_uid = lane_expected_uid(owners, lane);
        let lane_fd = open_lane_dir_fd(root_fd, lane, owners)?;
        let quarantine_fd = open_quarantine_dir_fd(root_fd, owners)?;

        let (append_lock, plan) = acquire_lock_and_phase1_unix(
            root_fd,
            &lane_fd,
            owners,
            lane_uid,
            DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT,
            observer,
        )?;

        // Strictly read-only pre-validation BEFORE any Phase 2 metadata repair
        // or `.tmp` cleanup: reject unconsumed/non-oldest/active segments and
        // reject missing/symlink/non-regular/foreign-owned target files with
        // zero disk mutation (even when `tail.meta` and `tail.meta.bak` diverge).
        validate_reclaim_plan_readonly(&plan, seg_id, highwater_seg)?;
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(
            OsStr::new(&seg_name),
            "reclaim sealed capture queue segment",
        )?;
        let target_expected_uid = if observer.simulate_foreign_reclaim_owner() {
            lane_uid.wrapping_add(1)
        } else {
            lane_uid
        };
        preverify_reclaim_target_unix(&lane_fd, &cname, target_expected_uid)?;

        let _cleaned_q_tmp =
            clean_lane_quarantine_tmp_files_unix(&quarantine_fd, lane, owners.quarantine_uid)?;
        let phase2_ctx = UnixPhase2Context {
            lane,
            lane_fd: &lane_fd,
            quarantine_fd: &quarantine_fd,
            quarantine_dir,
            owners,
            append_lock: &append_lock,
        };
        let converged = phase2_execute_plan_unix(&phase2_ctx, plan, observer)?;
        let next_meta = build_reclaimed_tail_meta(&converged.meta, seg_id, highwater_seg)?;

        // Step ① (`ADR-RB-05` #4 ③): atomically advance `MIN_LIVE_SEG` and
        // remove `seg_id` from `LTM2.sealed[]` across BOTH `tail.meta` and
        // `tail.meta.bak` strictly BEFORE unlinking `events-<seg_id:08>.log`.
        commit_dual_tail_meta_unix(&lane_fd, lane_label, &next_meta, lane_uid, observer)?;

        observer.maybe_crash(SegmentCrashPoint::AfterReclaimMetaBeforeUnlink)?;

        // Step ②: re-verify `events-<seg_id:08>.log` is an owned regular file
        // (never a symlink/dir), unlink it, and `fsync_dir(lane_dir)`.
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
        let stat_rc = unsafe {
            libc::fstatat(
                lane_fd.as_raw_fd(),
                cname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        let file_removed = if stat_rc == 0 {
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(map_queue_symlink_err("reclaimed capture queue segment"));
            }
            if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "reclaimed capture queue segment is not a regular file".to_string(),
                });
            }
            if st.st_uid != target_expected_uid {
                return Err(map_queue_owner_mismatch_err(
                    "reclaimed capture queue segment",
                ));
            }
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            let unlink_rc = unsafe { libc::unlinkat(lane_fd.as_raw_fd(), cname.as_ptr(), 0) };
            if unlink_rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(StorageBackendError::io(
                        "unlink reclaimed capture queue segment",
                        err,
                    ));
                }
                false
            } else {
                observer.record_reclaim_segment_unlink(lane_label, seg_id);
                fsync_fd(
                    &lane_fd,
                    "fsync capture queue lane directory after reclaiming sealed segment",
                )?;
                observer.record_fsync_lane_after_reclaim(lane_label, seg_id);
                true
            }
        } else {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "stat reclaimed capture queue segment",
                    err,
                ));
            }
            false
        };

        drop(append_lock);
        observer.record_release_lock();

        Ok(SegmentReclaimOutcome {
            lane,
            reclaimed_seg_id: seg_id,
            meta: next_meta,
            file_removed,
        })
    }

    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_in_storage_dir_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        highwater_seg: u32,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        let quarantine_dir = storage_dir
            .join(PRIVATE_SUBDIR)
            .join(CAPTURE_QUEUE_SUBDIR)
            .join(QUEUE_QUARANTINE_DIR);
        reclaim_under_root_fd_unix(
            &root_fd,
            &quarantine_dir,
            lane,
            seg_id,
            highwater_seg,
            owners,
            observer,
        )
    }

    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_at_root_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        highwater_seg: u32,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
        reclaim_under_root_fd_unix(
            &root_fd,
            &quarantine_dir,
            lane,
            seg_id,
            highwater_seg,
            owners,
            observer,
        )
    }

    pub(super) fn read_converged_lane_tail_meta_unix(
        lane_dir: &Path,
        expected_uid: u32,
    ) -> StorageResult<LaneTailMeta> {
        let (lane_fd, lane_st) =
            open_dir_nofollow(lane_dir, expected_uid, "capture queue lane directory")?;
        verify_dir_mode_0700(&lane_st, "capture queue lane directory")?;
        let primary = read_meta_replica_nofollow(
            &lane_fd,
            QUEUE_TAIL_META_FILE,
            expected_uid,
            "capture queue tail.meta",
        )?;
        let backup = read_meta_replica_nofollow(
            &lane_fd,
            QUEUE_TAIL_META_BAK_FILE,
            expected_uid,
            "capture queue tail.meta.bak",
        )?;
        match resolve_dual_replicas(primary, backup)? {
            ResolvedReplicas::Authoritative { meta, .. } => Ok(meta),
            ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => {
                Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "no valid LTM2 tail.meta replica present in capture queue lane"
                        .to_string(),
                })
            }
        }
    }
}

// ============================================================================
// Non-Unix Implementation (Windows Durable Parity; Fail-Closed Elsewhere)
// ============================================================================

#[cfg(not(unix))]
mod non_unix_seg {
    #[cfg(windows)]
    use std::{
        fs,
        os::windows::{
            ffi::OsStrExt as _,
            io::{AsRawHandle as _, FromRawHandle as _, RawHandle},
        },
    };

    use super::*;

    #[cfg(not(windows))]
    pub(super) fn read_converged_lane_tail_meta_non_unix(
        _lane_dir: &Path,
    ) -> StorageResult<LaneTailMeta> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue metadata read is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn publish_quarantine_durable_non_unix(
        _quarantine_dir: &Path,
        _id: &str,
        _bytes: &[u8],
        _observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue quarantine publish is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn quarantine_and_truncate_segment_non_unix(
        _quarantine_dir: &Path,
        _lane_dir: &Path,
        _lane: CaptureQueueLane,
        _seg_id: u32,
        _valid_end: u64,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue quarantine truncate is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn quarantine_and_unlink_orphan_non_unix(
        _quarantine_dir: &Path,
        _lane_dir: &Path,
        _lane: CaptureQueueLane,
        _orphan_seg_id: u32,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue quarantine unlink is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn append_in_storage_dir_non_unix(
        _storage_dir: &Path,
        _lane: CaptureQueueLane,
        _frame_buf: &[u8],
        _observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue segment append is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn append_at_root_non_unix(
        _queue_root: &Path,
        _lane: CaptureQueueLane,
        _frame_buf: &[u8],
        _observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue segment append is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn recover_in_storage_dir_non_unix(
        _storage_dir: &Path,
        _lane: CaptureQueueLane,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue lane recovery is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    pub(super) fn recover_at_root_non_unix(
        _queue_root: &Path,
        _lane: CaptureQueueLane,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue lane recovery is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_in_storage_dir_non_unix(
        _storage_dir: &Path,
        _lane: CaptureQueueLane,
        _seg_id: u32,
        _highwater_seg: u32,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue segment reclamation is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(not(windows))]
    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_at_root_non_unix(
        _queue_root: &Path,
        _lane: CaptureQueueLane,
        _seg_id: u32,
        _highwater_seg: u32,
        _observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue segment reclamation is unsupported on this platform"
                .to_string(),
        })
    }

    #[cfg(windows)]
    mod win_impl {
        #[cfg(test)]
        use windows_sys::Win32::Security::{
            CreateWellKnownSid, SECURITY_MAX_SID_SIZE, WinWorldSid,
        };
        use windows_sys::Win32::{
            Foundation::{
                CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
            },
            Security::{
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
                EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, TOKEN_INFORMATION_CLASS,
                TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, DELETE,
                FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo, GetFileInformationByHandle,
                OPEN_EXISTING, SetFileInformationByHandle,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };

        use super::*;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct WinFileIdentity {
            volume_serial: u32,
            index_high: u32,
            index_low: u32,
        }

        struct RawWinHandleGuard(HANDLE);

        impl Drop for RawWinHandleGuard {
            fn drop(&mut self) {
                // SAFETY: `self.0` is an exclusively owned valid Win32 handle.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        struct WinLocalAllocation(*mut std::ffi::c_void);

        impl Drop for WinLocalAllocation {
            fn drop(&mut self) {
                // SAFETY: `self.0` was allocated by a Win32 security API (`GetSecurityInfo`).
                unsafe {
                    LocalFree(self.0);
                }
            }
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        fn query_current_windows_token_info(
            class: TOKEN_INFORMATION_CLASS,
            context: &'static str,
        ) -> StorageResult<Vec<usize>> {
            // SAFETY: Win32 process token query with properly sized aligned buffer.
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return Err(StorageBackendError::io(
                        "open current Windows process token for capture queue owner check",
                        io::Error::last_os_error(),
                    ));
                }
                let _token_guard = RawWinHandleGuard(token);
                let mut size: u32 = 0;
                let _ = GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut size);
                if size == 0 {
                    return Err(StorageBackendError::io(context, io::Error::last_os_error()));
                }
                let mut buf = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
                if GetTokenInformation(token, class, buf.as_mut_ptr().cast(), size, &mut size) == 0
                {
                    return Err(StorageBackendError::io(context, io::Error::last_os_error()));
                }
                Ok(buf)
            }
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        fn verify_windows_handle_owner_against_sids(
            file: &File,
            allowed_sids: &[*mut std::ffi::c_void],
            subject: &'static str,
        ) -> StorageResult<()> {
            // SAFETY: `file` is a valid open Win32 handle opened with `GENERIC_READ`
            // (which includes `READ_CONTROL`); `descriptor` is freed via `WinLocalAllocation`.
            unsafe {
                let mut owner: *mut std::ffi::c_void = std::ptr::null_mut();
                let mut descriptor: *mut std::ffi::c_void = std::ptr::null_mut();
                let status = GetSecurityInfo(
                    file.as_raw_handle() as HANDLE,
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION,
                    &mut owner,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut descriptor,
                );
                if status != 0 {
                    return Err(StorageBackendError::io(
                        "query Windows owner SID for capture queue file",
                        io::Error::from_raw_os_error(status as i32),
                    ));
                }
                let _descriptor = WinLocalAllocation(descriptor);
                let matched = !owner.is_null()
                    && allowed_sids
                        .iter()
                        .any(|&sid| !sid.is_null() && EqualSid(owner, sid) != 0);
                if !matched {
                    return Err(map_queue_owner_mismatch_err(subject));
                }
            }
            Ok(())
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        fn verify_windows_reclaim_target_owner(
            file: &File,
            observer: &SegmentObserver,
        ) -> StorageResult<()> {
            #[cfg(test)]
            if observer.simulate_foreign_reclaim_owner() {
                let mut sid_buf = [0u8; SECURITY_MAX_SID_SIZE as usize];
                let mut sid_len = SECURITY_MAX_SID_SIZE;
                // SAFETY: `sid_buf` holds `SECURITY_MAX_SID_SIZE` bytes for `WinWorldSid`.
                let ok = unsafe {
                    CreateWellKnownSid(
                        WinWorldSid,
                        std::ptr::null_mut(),
                        sid_buf.as_mut_ptr().cast(),
                        &mut sid_len,
                    )
                };
                if ok == 0 {
                    return Err(StorageBackendError::io(
                        "construct Windows well-known SID for foreign owner test",
                        io::Error::last_os_error(),
                    ));
                }
                return verify_windows_handle_owner_against_sids(
                    file,
                    &[sid_buf.as_mut_ptr().cast()],
                    "reclaimed capture queue segment",
                );
            }
            #[cfg(not(test))]
            let _ = observer;

            // Accept either the process token's `TokenUser` SID or its default
            // `TokenOwner` SID (e.g., `BUILTIN\Administrators` in elevated tokens,
            // which Windows assigns to files created via `CreateFileW` with a
            // default security descriptor).
            let user_buf = query_current_windows_token_info(
                TokenUser,
                "query current Windows process token user for capture queue owner check",
            )?;
            let owner_buf = query_current_windows_token_info(
                TokenOwner,
                "query current Windows process token default owner for capture queue owner check",
            )?;
            // SAFETY: `user_buf` and `owner_buf` are aligned `TOKEN_USER`/`TOKEN_OWNER`
            // buffers from `GetTokenInformation` that stay alive through the check.
            let user_sid = unsafe { (&*user_buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
            let default_owner_sid = unsafe { (&*owner_buf.as_ptr().cast::<TOKEN_OWNER>()).Owner };
            verify_windows_handle_owner_against_sids(
                file,
                &[user_sid, default_owner_sid],
                "reclaimed capture queue segment",
            )
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        fn preverify_reclaim_target_windows(
            seg_path: &Path,
            observer: &SegmentObserver,
        ) -> StorageResult<()> {
            let (file, _, _) = match open_windows_file_nofollow(
                seg_path,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "reclaimed capture queue segment",
                "open reclaimed capture queue segment",
            ) {
                Ok(opened) => opened,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "reclaimed capture queue sealed segment is missing on disk"
                            .to_string(),
                    });
                }
                Err(err) => return Err(err),
            };
            verify_windows_reclaim_target_owner(&file, observer)?;
            Ok(())
        }

        /// Atomically mark an already-verified, handle-pinned Windows file
        /// (opened with `DELETE` access and without `FILE_SHARE_DELETE`) for
        /// deletion via `SetFileInformationByHandle(FileDispositionInfo)` and
        /// close it, eliminating any path-reopen TOCTOU replacement window.
        fn delete_windows_file_by_handle(file: File, context: &'static str) -> StorageResult<()> {
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            // SAFETY: `file` is a valid open Win32 handle opened with `DELETE`
            // access and without `FILE_SHARE_DELETE`; `info` matches
            // `FileDispositionInfo` layout.
            let ok = unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle() as HANDLE,
                    FileDispositionInfo,
                    (&raw const info).cast(),
                    u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>()).unwrap_or(1),
                )
            };
            if ok == 0 {
                return Err(StorageBackendError::io(context, io::Error::last_os_error()));
            }
            drop(file);
            Ok(())
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
            // SAFETY: `handle` is valid and `info` points to writable storage.
            let ok = unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) };
            if ok == 0 {
                return Err(StorageBackendError::io(context, io::Error::last_os_error()));
            }
            // SAFETY: `GetFileInformationByHandle` succeeded and initialized `info`.
            Ok(unsafe { info.assume_init() })
        }

        fn file_size_from_info(info: &BY_HANDLE_FILE_INFORMATION) -> u64 {
            ((u64::from(info.nFileSizeHigh)) << 32) | u64::from(info.nFileSizeLow)
        }

        fn file_identity_from_info(info: &BY_HANDLE_FILE_INFORMATION) -> WinFileIdentity {
            WinFileIdentity {
                volume_serial: info.dwVolumeSerialNumber,
                index_high: info.nFileIndexHigh,
                index_low: info.nFileIndexLow,
            }
        }

        /// Open and PIN a Windows directory handle with
        /// `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT` and
        /// `FILE_SHARE_READ | FILE_SHARE_WRITE` (explicitly omitting
        /// `FILE_SHARE_DELETE`), verifying directly on the opened handle that it
        /// is a real non-reparse directory. Holding the returned
        /// [`RawWinHandleGuard`] prevents directory rename/delete/junction-swap
        /// across the entire operation.
        fn pin_windows_dir_nofollow(
            path: &Path,
            subject: &'static str,
        ) -> StorageResult<RawWinHandleGuard> {
            let wide = to_wide_nul(path);
            // SAFETY: `wide` is valid NUL-terminated UTF-16 alive for the call.
            // Omitting `FILE_SHARE_DELETE` prevents concurrent rename/deletion
            // or junction substitution of this directory while pinned.
            let handle = unsafe {
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
                    "open capture queue directory handle",
                    io::Error::last_os_error(),
                ));
            }
            let guard = RawWinHandleGuard(handle);
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

        /// Open a Windows file handle strictly with `FILE_FLAG_OPEN_REPARSE_POINT`
        /// so symlinks/reparse points are NEVER followed, then verify on the
        /// opened handle via `GetFileInformationByHandle` that it is a regular
        /// non-reparse file, returning the pinned [`File`], exact size, and
        /// handle identity.
        fn open_windows_file_nofollow(
            path: &Path,
            desired_access: u32,
            share_mode: u32,
            creation: u32,
            subject: &'static str,
            context: &'static str,
        ) -> StorageResult<(File, u64, WinFileIdentity)> {
            let wide = to_wide_nul(path);
            // SAFETY: `wide` is valid NUL-terminated UTF-16 alive for the call.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    desired_access,
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
            // SAFETY: `handle` is an exclusively owned valid Win32 file handle.
            let file = unsafe { File::from_raw_handle(handle as RawHandle) };
            let info = file_info_by_handle(file.as_raw_handle() as HANDLE, context)?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: format!("{subject} is not a regular file"),
                });
            }
            let size = file_size_from_info(&info);
            let id = file_identity_from_info(&info);
            Ok((file, size, id))
        }

        /// Reject a reparse point or directory at `path` if `path` exists,
        /// strictly via `FILE_FLAG_OPEN_REPARSE_POINT` on an opened handle.
        fn ensure_not_reparse_or_dir_if_exists(
            path: &Path,
            subject: &'static str,
        ) -> StorageResult<bool> {
            let wide = to_wide_nul(path);
            // SAFETY: `wide` is valid NUL-terminated UTF-16 alive for the call.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::NotFound {
                    return Ok(false);
                }
                return Err(StorageBackendError::io(
                    "inspect capture queue file handle",
                    err,
                ));
            }
            let guard = RawWinHandleGuard(handle);
            let info = file_info_by_handle(guard.0, "stat capture queue file handle")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} is not a regular file"),
                });
            }
            Ok(true)
        }

        fn read_meta_replica_windows(
            lane_dir: &Path,
            file_name: &str,
            subject: &'static str,
        ) -> StorageResult<ReplicaState> {
            let path = lane_dir.join(file_name);
            let (file, file_len, _) = match open_windows_file_nofollow(
                &path,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                subject,
                "open capture queue metadata",
            ) {
                Ok(triple) => triple,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => return Ok(ReplicaState::Absent),
                Err(err) => return Err(err),
            };
            if file_len > LTM2_MAX_LEN as u64 {
                return Ok(ReplicaState::Corrupt);
            }
            let mut bytes = Vec::with_capacity((file_len as usize).min(LTM2_MAX_LEN + 1));
            file.take((LTM2_MAX_LEN + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| StorageBackendError::io("read capture queue metadata", e))?;
            if bytes.len() > LTM2_MAX_LEN {
                return Ok(ReplicaState::Corrupt);
            }
            match LaneTailMeta::decode(&bytes) {
                Ok(m) => Ok(ReplicaState::Valid(m)),
                Err(_) => Ok(ReplicaState::Corrupt),
            }
        }

        pub(super) fn read_converged_lane_tail_meta_windows(
            lane_dir: &Path,
        ) -> StorageResult<LaneTailMeta> {
            let _parent_guard = match lane_dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(parent) => Some(pin_windows_dir_nofollow(
                    parent,
                    "capture queue root directory",
                )?),
                None => None,
            };
            let _grandparent_guard = match lane_dir
                .parent()
                .and_then(Path::parent)
                .filter(|p| !p.as_os_str().is_empty())
            {
                Some(gp) if gp.file_name().and_then(|n| n.to_str()) == Some(PRIVATE_SUBDIR) => {
                    Some(pin_windows_dir_nofollow(gp, "capture private directory")?)
                }
                _ => None,
            };
            let _lane_guard = pin_windows_dir_nofollow(lane_dir, "capture queue lane directory")?;
            let primary = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_FILE,
                "capture queue tail.meta",
            )?;
            let backup = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_BAK_FILE,
                "capture queue tail.meta.bak",
            )?;
            match resolve_dual_replicas(primary, backup)? {
                ResolvedReplicas::Authoritative { meta, .. } => Ok(meta),
                ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => {
                    Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "no valid LTM2 tail.meta replica present in capture queue lane"
                            .to_string(),
                    })
                }
            }
        }

        fn scan_lane_dir_windows(lane_dir: &Path) -> StorageResult<(Vec<u32>, Vec<String>)> {
            let mut seg_ids = Vec::new();
            let mut stale_tmp_names = Vec::new();
            for entry in fs::read_dir(lane_dir)
                .map_err(|e| StorageBackendError::io("read capture queue lane directory", e))?
            {
                let entry = entry
                    .map_err(|e| StorageBackendError::io("read capture queue lane entry", e))?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "non-UTF-8 entry name in capture queue lane".to_string(),
                    });
                };
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
                } else if is_lane_meta_temp_name(name.as_bytes()) {
                    stale_tmp_names.push(name);
                }
            }
            seg_ids.sort_unstable();
            seg_ids.dedup();
            Ok((seg_ids, stale_tmp_names))
        }

        /// Pinned Windows segment handle validated directly by handle identity
        /// during Phase 1 and reused without path-reopen during Phase 2 append.
        struct PinnedWindowsSegment {
            file: File,
            identity: WinFileIdentity,
        }

        fn phase1_classify_lane_windows(
            lane_dir: &Path,
            open_seg_writable: bool,
        ) -> StorageResult<(Phase1Plan<String>, Option<PinnedWindowsSegment>)> {
            let (seg_ids, stale_tmp_names) = scan_lane_dir_windows(lane_dir)?;
            let primary = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_FILE,
                "capture queue tail.meta",
            )?;
            let backup = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_BAK_FILE,
                "capture queue tail.meta.bak",
            )?;
            let resolved = resolve_dual_replicas(primary, backup)?;

            if seg_ids.is_empty() {
                return match resolved {
                    ResolvedReplicas::BothAbsent => {
                        Ok((Phase1Plan::Case0aFreshEmpty { stale_tmp_names }, None))
                    }
                    ResolvedReplicas::Authoritative { meta, mirror_sync } => {
                        if meta.is_initial_empty() {
                            Ok((
                                Phase1Plan::Case0bPreInitializedEmpty {
                                    meta,
                                    mirror_sync,
                                    stale_tmp_names,
                                },
                                None,
                            ))
                        } else {
                            Err(StorageBackendError::CorruptCommittedFrame {
                                detail:
                                    "Case 0c/E: zero segment files present while LTM2 records non-empty committed state"
                                        .to_string(),
                            })
                        }
                    }
                    ResolvedReplicas::BothInvalidWithPresence => {
                        Err(StorageBackendError::CorruptCommittedFrame {
                            detail:
                                "Case 0d: zero segment files present with corrupt tail.meta replicas"
                                    .to_string(),
                        })
                    }
                };
            }

            let (meta, mirror_sync) = match resolved {
                ResolvedReplicas::Authoritative { meta, mirror_sync } => (meta, mirror_sync),
                ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail:
                            "Subcase B2: segment files exist while both LTM2 tail.meta replicas are absent or corrupt"
                                .to_string(),
                    });
                }
            };

            let mut reclaimed_orphans = Vec::new();
            let mut live_segs = Vec::new();
            for seg_id in seg_ids {
                if seg_id < meta.min_live_seg {
                    reclaimed_orphans.push(seg_id);
                } else {
                    live_segs.push(seg_id);
                }
            }
            let Some(&max_live_seg) = live_segs.last() else {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "Case E: no live segment files >= min_live_seg present".to_string(),
                });
            };
            if max_live_seg < meta.active_seg || live_segs.binary_search(&meta.active_seg).is_err()
            {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail:
                        "Case E: active segment missing or max live segment regressed below active_seg"
                            .to_string(),
                });
            }

            let seg_path = lane_dir.join(segment_log_file_name(meta.active_seg));
            let desired_access = if open_seg_writable {
                GENERIC_READ | GENERIC_WRITE
            } else {
                GENERIC_READ
            };
            let (mut seg_file, actual_len, seg_identity) = match open_windows_file_nofollow(
                &seg_path,
                desired_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue segment file",
                "open capture queue segment",
            ) {
                Ok(triple) => triple,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "Case E: active capture queue segment file is missing".to_string(),
                    });
                }
                Err(err) => return Err(err),
            };
            if actual_len < meta.committed_len {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "Case E: active segment file is shorter than LTM2 committed_len"
                        .to_string(),
                });
            }

            verify_committed_prefix_crc(&mut seg_file, meta.committed_len, meta.prefix_crc32c)?;

            if max_live_seg > meta.active_seg {
                let succ_seg = validate_single_case_d_successor_id(meta.active_seg, &live_segs)?;
                let succ_path = lane_dir.join(segment_log_file_name(succ_seg));
                let (mut succ_file, succ_actual_len, succ_identity) = open_windows_file_nofollow(
                    &succ_path,
                    desired_access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    OPEN_EXISTING,
                    "capture queue successor segment file",
                    "open Case D successor capture queue segment",
                )?;
                let succ_scan = scan_successor_segment_readonly(&mut succ_file, succ_actual_len)?;
                ensure_case_d_sealed_capacity(&meta, &succ_scan)?;
                let old_tail = if actual_len > meta.committed_len {
                    let tail = scan_case_c_uncommitted_tail(
                        &mut seg_file,
                        meta.committed_len,
                        actual_len,
                        meta.prefix_crc32c,
                    )?;
                    Some(NestedOldTailPlan {
                        valid_end: tail.valid_end,
                        recovered_crc32c: tail.recovered_crc32c,
                        frame_count: tail.frame_count,
                        has_damaged_tail: tail.has_damaged_tail,
                    })
                } else {
                    None
                };
                return match classify_case_d_readonly(
                    &meta,
                    old_tail.as_ref(),
                    succ_actual_len,
                    &succ_scan,
                )? {
                    CaseDPhase1Kind::ValidSuccessor { has_damaged_tail } => Ok((
                        Phase1Plan::CaseDRecoverValidSuccessor {
                            meta,
                            old_tail,
                            succ_seg,
                            succ_valid_end: succ_scan.valid_end,
                            succ_crc32c: succ_scan.valid_crc32c,
                            succ_frames: succ_scan.frame_count,
                            succ_has_damaged_tail: has_damaged_tail,
                            stale_tmp_names,
                            reclaimed_orphans,
                        },
                        Some(PinnedWindowsSegment {
                            file: succ_file,
                            identity: succ_identity,
                        }),
                    )),
                    CaseDPhase1Kind::OrphanSuccessor { non_empty } => {
                        // Drop the orphan handle immediately so Phase 2 can unlink it
                        // without a Windows sharing violation; keep the intact old
                        // `active_seg` handle pinned for subsequent appends.
                        drop(succ_file);
                        Ok((
                            Phase1Plan::CaseDCleanOrphanSuccessor {
                                meta,
                                old_tail,
                                orphan_seg: succ_seg,
                                orphan_non_empty: non_empty,
                                stale_tmp_names,
                                reclaimed_orphans,
                            },
                            Some(PinnedWindowsSegment {
                                file: seg_file,
                                identity: seg_identity,
                            }),
                        ))
                    }
                };
            }

            if actual_len == meta.committed_len {
                Ok((
                    Phase1Plan::CaseAClean {
                        meta,
                        mirror_sync,
                        stale_tmp_names,
                        reclaimed_orphans,
                    },
                    Some(PinnedWindowsSegment {
                        file: seg_file,
                        identity: seg_identity,
                    }),
                ))
            } else {
                let tail = scan_case_c_uncommitted_tail(
                    &mut seg_file,
                    meta.committed_len,
                    actual_len,
                    meta.prefix_crc32c,
                )?;
                Ok((
                    Phase1Plan::CaseCRecoverTail {
                        meta,
                        recovered_valid_end: tail.valid_end,
                        recovered_crc32c: tail.recovered_crc32c,
                        recovered_frames: tail.frame_count,
                        has_damaged_tail: tail.has_damaged_tail,
                        stale_tmp_names,
                        reclaimed_orphans,
                    },
                    Some(PinnedWindowsSegment {
                        file: seg_file,
                        identity: seg_identity,
                    }),
                ))
            }
        }

        fn lock_file_exclusive_with_deadline(
            file: &File,
            deadline: Instant,
            context: &'static str,
        ) -> StorageResult<()> {
            acquire_lock_with_deadline_loop(
                deadline,
                context,
                || match file.try_lock() {
                    Ok(()) => TryLockOutcome::Acquired,
                    Err(std::fs::TryLockError::WouldBlock) => TryLockOutcome::WouldBlock,
                    Err(std::fs::TryLockError::Error(err)) => TryLockOutcome::Error(err),
                },
                Instant::now,
                std::thread::sleep,
            )
        }

        fn acquire_existing_lock_windows(
            queue_root: &Path,
            deadline: Instant,
        ) -> StorageResult<Option<File>> {
            let lock_path = queue_root.join(QUEUE_APPEND_LOCK_FILE);
            match open_windows_file_nofollow(
                &lock_path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue append.lock",
                "open capture queue append.lock",
            ) {
                Ok((file, _, _)) => {
                    lock_file_exclusive_with_deadline(
                        &file,
                        deadline,
                        "lock capture queue append.lock",
                    )?;
                    Ok(Some(file))
                }
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => Ok(None),
                Err(err) => Err(err),
            }
        }

        fn create_or_open_lock_windows(
            queue_root: &Path,
            deadline: Instant,
        ) -> StorageResult<File> {
            let lock_path = queue_root.join(QUEUE_APPEND_LOCK_FILE);
            for _ in 0..32 {
                if let Some(file) = acquire_existing_lock_windows(queue_root, deadline)? {
                    return Ok(file);
                }
                match open_windows_file_nofollow(
                    &lock_path,
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    CREATE_NEW,
                    "capture queue append.lock",
                    "create capture queue append.lock",
                ) {
                    Ok((file, _, _)) => {
                        lock_file_exclusive_with_deadline(
                            &file,
                            deadline,
                            "lock capture queue append.lock",
                        )?;
                        flush_windows_dir(queue_root)?;
                        return Ok(file);
                    }
                    Err(StorageBackendError::Io {
                        kind: io::ErrorKind::AlreadyExists,
                        ..
                    }) => continue,
                    Err(err) => return Err(err),
                }
            }
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue append.lock open/create contended past retry bound"
                    .to_string(),
            })
        }

        fn acquire_lock_and_phase1_windows(
            queue_root: &Path,
            lane_dir: &Path,
            open_seg_writable: bool,
            lock_timeout: Duration,
            observer: &mut SegmentObserver,
        ) -> StorageResult<(File, Phase1Plan<String>, Option<PinnedWindowsSegment>)> {
            let deadline = lock_deadline_for_timeout(lock_timeout);
            if let Some(lock) = acquire_existing_lock_windows(queue_root, deadline)? {
                observer.record_acquire_lock();
                let (plan, pinned) = phase1_classify_lane_windows(lane_dir, open_seg_writable)?;
                return Ok((lock, plan, pinned));
            }
            // `append.lock` is absent: pre-classify read-only first so corrupt
            // lanes exit without creating `append.lock`.
            let _ = phase1_classify_lane_windows(lane_dir, false)?;
            let lock = create_or_open_lock_windows(queue_root, deadline)?;
            observer.record_acquire_lock();
            let (plan, pinned) = phase1_classify_lane_windows(lane_dir, open_seg_writable)?;
            Ok((lock, plan, pinned))
        }

        fn commit_single_meta_replica_windows(
            lane_dir: &Path,
            lane_label: &'static str,
            tmp_name: &'static str,
            target_name: &'static str,
            encoded_bytes: &[u8],
            observer: &mut SegmentObserver,
        ) -> StorageResult<()> {
            let tmp_path = lane_dir.join(tmp_name);
            let target_path = lane_dir.join(target_name);
            let _ =
                ensure_not_reparse_or_dir_if_exists(&target_path, "capture queue tail metadata")?;

            let (mut tmp_file, _, _) = open_windows_file_nofollow(
                &tmp_path,
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                CREATE_NEW,
                "capture queue temp metadata",
                "create capture queue temp metadata",
            )?;
            if let Err(err) = tmp_file
                .write_all(encoded_bytes)
                .and_then(|()| tmp_file.sync_data())
            {
                drop(tmp_file);
                let _ = fs::remove_file(&tmp_path);
                return Err(StorageBackendError::io("write LTM2 temp metadata", err));
            }
            drop(tmp_file);
            observer.record_meta_tmp_synced(lane_label, tmp_name, target_name);

            let before_rename_crash = if target_name == QUEUE_TAIL_META_FILE {
                SegmentCrashPoint::BeforePrimaryMetaRename
            } else {
                SegmentCrashPoint::BeforeBackupMetaRename
            };
            observer.maybe_crash(before_rename_crash)?;

            if let Err(err) = durable_replace_publish_windows(&tmp_path, &target_path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
            observer.record_meta_renamed(lane_label, tmp_name, target_name);
            flush_windows_dir(lane_dir)?;
            observer.record_fsync_lane_after_meta(lane_label, target_name);

            if target_name == QUEUE_TAIL_META_FILE {
                observer.maybe_crash(SegmentCrashPoint::AfterPrimaryMetaCommit)?;
            }
            Ok(())
        }

        fn commit_dual_tail_meta_windows(
            lane_dir: &Path,
            lane_label: &'static str,
            meta: &LaneTailMeta,
            observer: &mut SegmentObserver,
        ) -> StorageResult<()> {
            let encoded = meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
            commit_single_meta_replica_windows(
                lane_dir,
                lane_label,
                QUEUE_TAIL_META_TMP_FILE,
                QUEUE_TAIL_META_FILE,
                &encoded,
                observer,
            )?;
            commit_single_meta_replica_windows(
                lane_dir,
                lane_label,
                QUEUE_TAIL_META_BAK_TMP_FILE,
                QUEUE_TAIL_META_BAK_FILE,
                &encoded,
                observer,
            )?;
            Ok(())
        }

        struct WindowsPhase2Context<'a> {
            lane: CaptureQueueLane,
            lane_dir: &'a Path,
            quarantine_dir: &'a Path,
            append_lock: &'a File,
        }

        struct WindowsPhase2ConvergedState {
            meta: LaneTailMeta,
            active_seg_exists: bool,
            recovered_frames: usize,
            recovered_bytes: u64,
            mirrored_replica: bool,
            cleaned_tmp_files: usize,
            reclaimed_orphans_removed: usize,
            quarantined_artifacts: Vec<QuarantinePublishReport>,
            lane_dir_synced: bool,
            /// Freshly created active segment handle when Step C2 rotates the
            /// lane to `S + 1` during Phase 2, replacing the Phase 1 handle
            /// pinned to old segment `S`.
            replacement_active_seg: Option<PinnedWindowsSegment>,
        }

        struct WindowsCaseCStepC1Outcome {
            meta: LaneTailMeta,
            recovered_bytes: u64,
            quarantined_artifact: Option<QuarantinePublishReport>,
        }

        /// Shared Windows `ADR-RB-05` #3 Case C Step C1 production routine (`RB-08m`).
        fn case_c_step_c1_windows(
            ctx: &WindowsPhase2Context<'_>,
            meta: LaneTailMeta,
            recovered_valid_end: u64,
            recovered_crc32c: u32,
            recovered_frames: usize,
            has_damaged_tail: bool,
            observer: &mut SegmentObserver,
        ) -> StorageResult<WindowsCaseCStepC1Outcome> {
            let lane_dir = ctx.lane_dir;
            let lane_label = lane_dir_name(ctx.lane);
            let recovered_bytes = recovered_valid_end
                .checked_sub(meta.committed_len)
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "case C recovered length underflow".to_string(),
                })?;

            let quarantined_artifact = if has_damaged_tail {
                let report = quarantine_and_truncate_segment_windows_locked(
                    ctx.quarantine_dir,
                    lane_dir,
                    ctx.lane,
                    meta.active_seg,
                    recovered_valid_end,
                    ctx.append_lock,
                    observer,
                )?;
                observer.maybe_crash(SegmentCrashPoint::AfterCaseCStepC1TruncateSyncBeforeMeta)?;
                Some(report)
            } else {
                None
            };

            observer.record_case_c_recover(
                lane_label,
                meta.active_seg,
                meta.committed_len,
                recovered_valid_end,
                recovered_frames,
            );
            let next_meta = LaneTailMeta {
                min_live_seg: meta.min_live_seg,
                active_seg: meta.active_seg,
                committed_len: recovered_valid_end,
                prefix_crc32c: recovered_crc32c,
                sealed: meta.sealed,
            };
            commit_dual_tail_meta_windows(lane_dir, lane_label, &next_meta, observer)?;

            Ok(WindowsCaseCStepC1Outcome {
                meta: next_meta,
                recovered_bytes,
                quarantined_artifact,
            })
        }

        /// Base Windows `ADR-RB-05` #3 Case C **Step C2** production routine
        /// (`RB-08n` AC1–AC5): reuses locked `create_segment_file_windows_locked`
        /// to create `events-<S+1:08>.log` (`CREATE_NEW` + `flush_windows_dir`)
        /// and atomically commits dual-replica `LTM2`, returning the pinned
        /// handle for the newly created segment `S + 1`.
        fn seal_recovered_segment_to_next_windows(
            ctx: &WindowsPhase2Context<'_>,
            step_c1_meta: &LaneTailMeta,
            observer: &mut SegmentObserver,
        ) -> StorageResult<(LaneTailMeta, PinnedWindowsSegment)> {
            let next_meta = build_case_c_step_c2_meta(step_c1_meta)?;
            let lane_label = lane_dir_name(ctx.lane);
            let (new_seg_file, new_seg_identity) = create_segment_file_windows_locked(
                ctx.lane_dir,
                lane_label,
                next_meta.active_seg,
                ctx.append_lock,
                observer,
            )?;
            observer.maybe_crash(SegmentCrashPoint::AfterStepC2CreateSegmentBeforeMeta)?;
            commit_dual_tail_meta_windows(ctx.lane_dir, lane_label, &next_meta, observer)?;
            Ok((
                next_meta,
                PinnedWindowsSegment {
                    file: new_seg_file,
                    identity: new_seg_identity,
                },
            ))
        }

        fn phase2_execute_plan_windows(
            ctx: &WindowsPhase2Context<'_>,
            plan: Phase1Plan<String>,
            observer: &mut SegmentObserver,
        ) -> StorageResult<WindowsPhase2ConvergedState> {
            let lane_dir = ctx.lane_dir;
            let lane_label = lane_dir_name(ctx.lane);
            let clean_tmp = |names: &[String]| -> StorageResult<usize> {
                let mut removed = 0usize;
                for name in names {
                    let p = lane_dir.join(name);
                    if ensure_not_reparse_or_dir_if_exists(&p, "capture queue temp metadata")? {
                        fs::remove_file(&p).map_err(|e| {
                            StorageBackendError::io("remove stale capture queue temp metadata", e)
                        })?;
                        removed += 1;
                    }
                }
                if removed > 0 {
                    flush_windows_dir(lane_dir)?;
                }
                Ok(removed)
            };
            let clean_orphans = |orphans: &[u32]| -> StorageResult<usize> {
                let mut removed = 0usize;
                for &seg_id in orphans {
                    let p = lane_dir.join(segment_log_file_name(seg_id));
                    if ensure_not_reparse_or_dir_if_exists(&p, "capture queue reclaimed orphan")? {
                        fs::remove_file(&p).map_err(|e| {
                            StorageBackendError::io("remove reclaimed capture queue orphan", e)
                        })?;
                        removed += 1;
                    }
                }
                if removed > 0 {
                    flush_windows_dir(lane_dir)?;
                }
                Ok(removed)
            };

            match plan {
                Phase1Plan::Case0aFreshEmpty { stale_tmp_names } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let meta = LaneTailMeta::initial_empty();
                    commit_dual_tail_meta_windows(lane_dir, lane_label, &meta, observer)?;
                    Ok(WindowsPhase2ConvergedState {
                        meta,
                        active_seg_exists: false,
                        recovered_frames: 0,
                        recovered_bytes: 0,
                        mirrored_replica: false,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: 0,
                        quarantined_artifacts: Vec::new(),
                        lane_dir_synced: true,
                        replacement_active_seg: None,
                    })
                }
                Phase1Plan::Case0bPreInitializedEmpty {
                    meta,
                    mirror_sync,
                    stale_tmp_names,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let mirrored = match mirror_sync {
                        MirrorSync::None => false,
                        MirrorSync::SyncBackup => {
                            let encoded =
                                meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                            commit_single_meta_replica_windows(
                                lane_dir,
                                lane_label,
                                QUEUE_TAIL_META_BAK_TMP_FILE,
                                QUEUE_TAIL_META_BAK_FILE,
                                &encoded,
                                observer,
                            )?;
                            true
                        }
                        MirrorSync::SyncPrimary => {
                            let encoded =
                                meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                            commit_single_meta_replica_windows(
                                lane_dir,
                                lane_label,
                                QUEUE_TAIL_META_TMP_FILE,
                                QUEUE_TAIL_META_FILE,
                                &encoded,
                                observer,
                            )?;
                            true
                        }
                    };
                    Ok(WindowsPhase2ConvergedState {
                        meta,
                        active_seg_exists: false,
                        recovered_frames: 0,
                        recovered_bytes: 0,
                        mirrored_replica: mirrored,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: 0,
                        quarantined_artifacts: Vec::new(),
                        lane_dir_synced: mirrored || cleaned > 0,
                        replacement_active_seg: None,
                    })
                }
                Phase1Plan::CaseAClean {
                    meta,
                    mirror_sync,
                    stale_tmp_names,
                    reclaimed_orphans,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let orphans = clean_orphans(&reclaimed_orphans)?;
                    let mirrored = match mirror_sync {
                        MirrorSync::None => false,
                        MirrorSync::SyncBackup => {
                            let encoded =
                                meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                            commit_single_meta_replica_windows(
                                lane_dir,
                                lane_label,
                                QUEUE_TAIL_META_BAK_TMP_FILE,
                                QUEUE_TAIL_META_BAK_FILE,
                                &encoded,
                                observer,
                            )?;
                            true
                        }
                        MirrorSync::SyncPrimary => {
                            let encoded =
                                meta.encode().map_err(Ltm2CodecError::into_storage_error)?;
                            commit_single_meta_replica_windows(
                                lane_dir,
                                lane_label,
                                QUEUE_TAIL_META_TMP_FILE,
                                QUEUE_TAIL_META_FILE,
                                &encoded,
                                observer,
                            )?;
                            true
                        }
                    };
                    Ok(WindowsPhase2ConvergedState {
                        meta,
                        active_seg_exists: true,
                        recovered_frames: 0,
                        recovered_bytes: 0,
                        mirrored_replica: mirrored,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: orphans,
                        quarantined_artifacts: Vec::new(),
                        lane_dir_synced: mirrored || cleaned > 0 || orphans > 0,
                        replacement_active_seg: None,
                    })
                }
                Phase1Plan::CaseCRecoverTail {
                    meta,
                    recovered_valid_end,
                    recovered_crc32c,
                    recovered_frames,
                    has_damaged_tail,
                    stale_tmp_names,
                    reclaimed_orphans,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let orphans = clean_orphans(&reclaimed_orphans)?;
                    let step_c1 = case_c_step_c1_windows(
                        ctx,
                        meta,
                        recovered_valid_end,
                        recovered_crc32c,
                        recovered_frames,
                        has_damaged_tail,
                        observer,
                    )?;
                    // `RB-08q`: Skip Step C2 when `recovered_valid_end == 0`
                    // (fresh segment first-frame crash) to avoid sealing 0-byte segments.
                    let (final_meta, replacement_active_seg) = if has_damaged_tail
                        && step_c1.meta.committed_len > 0
                    {
                        observer
                            .maybe_crash(SegmentCrashPoint::AfterCaseCStepC1MetaBeforeStepC2)?;
                        let (c2_meta, pinned_next) =
                            seal_recovered_segment_to_next_windows(ctx, &step_c1.meta, observer)?;
                        (c2_meta, Some(pinned_next))
                    } else {
                        (step_c1.meta, None)
                    };
                    let quarantined_artifacts = step_c1.quarantined_artifact.into_iter().collect();
                    Ok(WindowsPhase2ConvergedState {
                        meta: final_meta,
                        active_seg_exists: true,
                        recovered_frames,
                        recovered_bytes: step_c1.recovered_bytes,
                        mirrored_replica: true,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: orphans,
                        quarantined_artifacts,
                        lane_dir_synced: true,
                        replacement_active_seg,
                    })
                }
                Phase1Plan::CaseDRecoverValidSuccessor {
                    meta,
                    old_tail,
                    succ_seg,
                    succ_valid_end,
                    succ_crc32c,
                    succ_frames,
                    succ_has_damaged_tail,
                    stale_tmp_names,
                    reclaimed_orphans,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let orphans = clean_orphans(&reclaimed_orphans)?;
                    let mut quarantined_artifacts = Vec::new();
                    let (effective_old_meta, old_recovered_frames, old_recovered_bytes) =
                        if let Some(ot) = old_tail {
                            let step_c1 = case_c_step_c1_windows(
                                ctx,
                                meta,
                                ot.valid_end,
                                ot.recovered_crc32c,
                                ot.frame_count,
                                ot.has_damaged_tail,
                                observer,
                            )?;
                            if let Some(q_rep) = step_c1.quarantined_artifact {
                                quarantined_artifacts.push(q_rep);
                            }
                            (step_c1.meta, ot.frame_count, step_c1.recovered_bytes)
                        } else {
                            (meta, 0, 0)
                        };
                    if succ_has_damaged_tail {
                        let q_report = quarantine_and_truncate_segment_windows_locked(
                            ctx.quarantine_dir,
                            lane_dir,
                            ctx.lane,
                            succ_seg,
                            succ_valid_end,
                            ctx.append_lock,
                            observer,
                        )?;
                        quarantined_artifacts.push(q_report);
                    }
                    let next_meta = build_case_d_sealed_successor_meta(
                        &effective_old_meta,
                        succ_seg,
                        succ_valid_end,
                        succ_crc32c,
                    )?;
                    observer.record_case_d_recover_successor(
                        lane_label,
                        effective_old_meta.active_seg,
                        succ_seg,
                        succ_valid_end,
                        succ_frames,
                    );
                    commit_dual_tail_meta_windows(lane_dir, lane_label, &next_meta, observer)?;
                    Ok(WindowsPhase2ConvergedState {
                        meta: next_meta,
                        active_seg_exists: true,
                        recovered_frames: old_recovered_frames + succ_frames,
                        recovered_bytes: old_recovered_bytes + succ_valid_end,
                        mirrored_replica: true,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: orphans,
                        quarantined_artifacts,
                        lane_dir_synced: true,
                        replacement_active_seg: None,
                    })
                }
                Phase1Plan::CaseDCleanOrphanSuccessor {
                    meta,
                    old_tail,
                    orphan_seg,
                    orphan_non_empty,
                    stale_tmp_names,
                    reclaimed_orphans,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let orphans = clean_orphans(&reclaimed_orphans)?;
                    let mut quarantined_artifacts = Vec::new();
                    let (effective_old_meta, old_recovered_frames, old_recovered_bytes) =
                        if let Some(ot) = old_tail {
                            let step_c1 = case_c_step_c1_windows(
                                ctx,
                                meta,
                                ot.valid_end,
                                ot.recovered_crc32c,
                                ot.frame_count,
                                ot.has_damaged_tail,
                                observer,
                            )?;
                            if let Some(q_rep) = step_c1.quarantined_artifact {
                                quarantined_artifacts.push(q_rep);
                            }
                            (step_c1.meta, ot.frame_count, step_c1.recovered_bytes)
                        } else {
                            (meta, 0, 0)
                        };
                    if orphan_non_empty {
                        let q_report = quarantine_and_unlink_orphan_windows_locked(
                            ctx.quarantine_dir,
                            lane_dir,
                            ctx.lane,
                            orphan_seg,
                            ctx.append_lock,
                            observer,
                        )?;
                        quarantined_artifacts.push(q_report);
                    } else {
                        unlink_empty_orphan_windows(lane_dir, ctx.lane, orphan_seg, observer)?;
                    }
                    commit_dual_tail_meta_windows(
                        lane_dir,
                        lane_label,
                        &effective_old_meta,
                        observer,
                    )?;
                    Ok(WindowsPhase2ConvergedState {
                        meta: effective_old_meta,
                        active_seg_exists: true,
                        recovered_frames: old_recovered_frames,
                        recovered_bytes: old_recovered_bytes,
                        mirrored_replica: true,
                        cleaned_tmp_files: cleaned,
                        reclaimed_orphans_removed: orphans,
                        quarantined_artifacts,
                        lane_dir_synced: true,
                        replacement_active_seg: None,
                    })
                }
            }
        }

        fn unlink_empty_orphan_windows(
            lane_dir: &Path,
            lane: CaptureQueueLane,
            orphan_seg_id: u32,
            observer: &mut SegmentObserver,
        ) -> StorageResult<()> {
            let orphan_path = lane_dir.join(segment_log_file_name(orphan_seg_id));
            let (orphan_file, actual_len, _) = open_windows_file_nofollow(
                &orphan_path,
                GENERIC_READ | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue empty orphan segment",
                "open empty orphan segment",
            )?;
            if actual_len != 0 {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "expected 0-byte empty orphan segment became non-empty before unlink"
                        .to_string(),
                });
            }
            let lane_label = lane_dir_name(lane);
            observer.record_orphan_unlink_start(lane_label, orphan_seg_id);
            delete_windows_file_by_handle(
                orphan_file,
                "unlink empty capture queue orphan segment",
            )?;
            observer.record_orphan_unlink_complete(lane_label, orphan_seg_id);
            observer.maybe_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink)?;
            flush_windows_dir(lane_dir)?;
            observer.record_fsync_lane_after_orphan_unlink(lane_label, orphan_seg_id);
            Ok(())
        }

        fn clean_lane_quarantine_tmp_files_windows(
            quarantine_dir: &Path,
            lane: CaptureQueueLane,
        ) -> StorageResult<usize> {
            let mut removed = 0usize;
            for entry in fs::read_dir(quarantine_dir).map_err(|e| {
                StorageBackendError::io("read capture queue quarantine directory", e)
            })? {
                let entry = entry.map_err(|e| {
                    StorageBackendError::io("read capture queue quarantine entry", e)
                })?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if is_lane_quarantine_temp_name(lane, name.as_bytes()) {
                    let p = quarantine_dir.join(&name);
                    if ensure_not_reparse_or_dir_if_exists(&p, "capture queue quarantine temp")? {
                        fs::remove_file(&p).map_err(|e| {
                            StorageBackendError::io("remove stale quarantine temp file", e)
                        })?;
                        removed += 1;
                    }
                }
            }
            if removed > 0 {
                flush_windows_dir(quarantine_dir)?;
            }
            Ok(removed)
        }

        fn pin_windows_dir_and_ancestors(
            dir: &Path,
            subject: &'static str,
        ) -> StorageResult<Vec<RawWinHandleGuard>> {
            let mut guards = Vec::with_capacity(3);
            if let Some(gp) = dir
                .parent()
                .and_then(Path::parent)
                .filter(|p| !p.as_os_str().is_empty())
                && gp.file_name().and_then(|n| n.to_str()) == Some(PRIVATE_SUBDIR)
            {
                guards.push(pin_windows_dir_nofollow(gp, "capture private directory")?);
            }
            if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                guards.push(pin_windows_dir_nofollow(
                    parent,
                    "capture queue root directory",
                )?);
            }
            guards.push(pin_windows_dir_nofollow(dir, subject)?);
            Ok(guards)
        }

        fn acquire_quarantine_lock_windows(
            quarantine_dir: &Path,
        ) -> StorageResult<(Vec<RawWinHandleGuard>, File)> {
            let queue_root = quarantine_dir
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "capture queue quarantine directory has no parent queue root"
                        .to_string(),
                })?;
            let guards = pin_windows_dir_and_ancestors(
                quarantine_dir,
                "capture queue quarantine directory",
            )?;
            let deadline = lock_deadline_for_timeout(DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT);
            let append_lock = match acquire_existing_lock_windows(queue_root, deadline)? {
                Some(lock) => lock,
                None => create_or_open_lock_windows(queue_root, deadline)?,
            };
            Ok((guards, append_lock))
        }

        pub(super) fn publish_quarantine_durable_windows(
            quarantine_dir: &Path,
            id: &str,
            bytes: &[u8],
            observer: &mut SegmentObserver,
        ) -> StorageResult<QuarantinePublishReport> {
            validate_quarantine_artifact_id(id)?;
            let (_q_guards, append_lock) = acquire_quarantine_lock_windows(quarantine_dir)?;
            let len_u64 =
                u64::try_from(bytes.len()).map_err(|_| StorageBackendError::BackendInternal {
                    detail: "quarantine payload length exceeds u64".to_string(),
                })?;
            let crc = crc32c_checksum(bytes);
            publish_quarantine_durable_writer_windows_pinned(
                quarantine_dir,
                id,
                &append_lock,
                observer,
                |tmp_file| {
                    tmp_file
                        .write_all(bytes)
                        .map_err(|e| StorageBackendError::io("write quarantine temp bytes", e))?;
                    Ok((len_u64, crc))
                },
            )
        }

        fn publish_quarantine_durable_writer_windows_pinned<F>(
            quarantine_dir: &Path,
            id: &str,
            _append_lock: &File,
            observer: &mut SegmentObserver,
            write_fn: F,
        ) -> StorageResult<QuarantinePublishReport>
        where
            F: FnOnce(&mut File) -> StorageResult<(u64, u32)>,
        {
            let tmp_path = quarantine_dir.join(format!("{id}.tmp"));
            let bin_path = quarantine_dir.join(format!("{id}.bin"));
            let _ = ensure_not_reparse_or_dir_if_exists(
                &bin_path,
                "capture queue quarantine artifact",
            )?;
            if ensure_not_reparse_or_dir_if_exists(&tmp_path, "capture queue quarantine temp")? {
                fs::remove_file(&tmp_path)
                    .map_err(|e| StorageBackendError::io("remove stale quarantine temp file", e))?;
            }
            let (mut tmp_file, _, _) = open_windows_file_nofollow(
                &tmp_path,
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                CREATE_NEW,
                "capture queue quarantine temp",
                "create quarantine temp file",
            )?;
            let (quarantined_bytes, crc32c) = match write_fn(&mut tmp_file) {
                Ok(res) => res,
                Err(err) => {
                    drop(tmp_file);
                    let _ = fs::remove_file(&tmp_path);
                    return Err(err);
                }
            };
            observer.record_quarantine_tmp_sync_start(id);
            if let Err(err) = tmp_file.sync_data() {
                drop(tmp_file);
                let _ = fs::remove_file(&tmp_path);
                return Err(StorageBackendError::io("sync quarantine temp data", err));
            }
            drop(tmp_file);
            observer.record_quarantine_tmp_sync_complete(id);

            observer.maybe_crash(SegmentCrashPoint::BeforeQuarantineRename)?;

            observer.record_quarantine_rename_start(id);
            if let Err(err) = durable_replace_publish_windows(&tmp_path, &bin_path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
            observer.record_quarantine_rename_complete(id);

            observer.record_quarantine_fsync_dir_start(id);
            flush_windows_dir(quarantine_dir)?;
            observer.record_quarantine_fsync_dir_complete(id);

            observer.maybe_crash(SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive)?;

            Ok(QuarantinePublishReport {
                id: id.to_string(),
                bin_path,
                quarantined_bytes,
                crc32c,
                diagnostic_code: QUARANTINE_DIAGNOSTIC_CODE,
            })
        }

        fn quarantine_and_truncate_segment_windows_locked(
            quarantine_dir: &Path,
            lane_dir: &Path,
            lane: CaptureQueueLane,
            seg_id: u32,
            valid_end: u64,
            append_lock: &File,
            observer: &mut SegmentObserver,
        ) -> StorageResult<QuarantinePublishReport> {
            let seg_path = lane_dir.join(segment_log_file_name(seg_id));
            let (mut seg_file, actual_len, _) = open_windows_file_nofollow(
                &seg_path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue segment file",
                "open segment for quarantine truncate",
            )?;
            if valid_end > actual_len {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "truncate valid_end exceeds segment actual length".to_string(),
                });
            }
            let tail_len = actual_len - valid_end;
            if tail_len == 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: "quarantine_and_truncate_segment requires non-empty damaged tail"
                        .to_string(),
                });
            }
            let expected_crc = stream_crc32c_file_range(&mut seg_file, valid_end, tail_len)?;
            let id = quarantine_artifact_id_from_crc(lane, seg_id, valid_end, expected_crc);
            let report = publish_quarantine_durable_writer_windows_pinned(
                quarantine_dir,
                &id,
                append_lock,
                observer,
                |tmp_file| {
                    let copied_crc = stream_copy_file_range_with_crc(
                        &mut seg_file,
                        valid_end,
                        tail_len,
                        tmp_file,
                    )?;
                    if copied_crc != expected_crc {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail:
                                "capture queue segment tail changed while streaming to quarantine"
                                    .to_string(),
                        });
                    }
                    Ok((tail_len, copied_crc))
                },
            )?;
            let lane_label = lane_dir_name(lane);
            observer.record_segment_truncate_start(lane_label, seg_id, valid_end);
            seg_file
                .set_len(valid_end)
                .map_err(|e| StorageBackendError::io("truncate capture queue segment", e))?;
            observer.record_segment_truncate_complete(lane_label, seg_id, valid_end);
            seg_file
                .sync_data()
                .map_err(|e| StorageBackendError::io("sync truncated capture queue segment", e))?;
            observer.record_segment_sync_data(lane_label, seg_id);
            Ok(report)
        }

        pub(super) fn quarantine_and_truncate_segment_windows(
            quarantine_dir: &Path,
            lane_dir: &Path,
            lane: CaptureQueueLane,
            seg_id: u32,
            valid_end: u64,
            observer: &mut SegmentObserver,
        ) -> StorageResult<QuarantinePublishReport> {
            if quarantine_dir.parent() != lane_dir.parent() {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "quarantine and lane directories must share the same capture queue root"
                            .to_string(),
                });
            }
            let (_q_guards, append_lock) = acquire_quarantine_lock_windows(quarantine_dir)?;
            let _l_guard = pin_windows_dir_nofollow(lane_dir, "capture queue lane directory")?;
            quarantine_and_truncate_segment_windows_locked(
                quarantine_dir,
                lane_dir,
                lane,
                seg_id,
                valid_end,
                &append_lock,
                observer,
            )
        }

        fn quarantine_and_unlink_orphan_windows_locked(
            quarantine_dir: &Path,
            lane_dir: &Path,
            lane: CaptureQueueLane,
            orphan_seg_id: u32,
            append_lock: &File,
            observer: &mut SegmentObserver,
        ) -> StorageResult<QuarantinePublishReport> {
            let orphan_path = lane_dir.join(segment_log_file_name(orphan_seg_id));
            let (mut orphan_file, actual_len, _) = open_windows_file_nofollow(
                &orphan_path,
                GENERIC_READ | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "capture queue orphan segment file",
                "open orphan segment for quarantine",
            )?;
            if actual_len == 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: "quarantine_and_unlink_orphan requires non-empty orphan segment"
                        .to_string(),
                });
            }
            let expected_crc = stream_crc32c_file_range(&mut orphan_file, 0, actual_len)?;
            let id = quarantine_artifact_id_from_crc(lane, orphan_seg_id, 0, expected_crc);
            let report = publish_quarantine_durable_writer_windows_pinned(
                quarantine_dir,
                &id,
                append_lock,
                observer,
                |tmp_file| {
                    let copied_crc =
                        stream_copy_file_range_with_crc(&mut orphan_file, 0, actual_len, tmp_file)?;
                    if copied_crc != expected_crc {
                        return Err(StorageBackendError::CorruptCommittedFrame {
                            detail:
                                "capture queue orphan segment changed while streaming to quarantine"
                                    .to_string(),
                        });
                    }
                    Ok((actual_len, copied_crc))
                },
            )?;
            let lane_label = lane_dir_name(lane);
            observer.record_orphan_unlink_start(lane_label, orphan_seg_id);
            delete_windows_file_by_handle(
                orphan_file,
                "unlink quarantined capture queue orphan segment",
            )?;
            observer.record_orphan_unlink_complete(lane_label, orphan_seg_id);
            observer.maybe_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink)?;
            flush_windows_dir(lane_dir)?;
            observer.record_fsync_lane_after_orphan_unlink(lane_label, orphan_seg_id);
            Ok(report)
        }

        pub(super) fn quarantine_and_unlink_orphan_windows(
            quarantine_dir: &Path,
            lane_dir: &Path,
            lane: CaptureQueueLane,
            orphan_seg_id: u32,
            observer: &mut SegmentObserver,
        ) -> StorageResult<QuarantinePublishReport> {
            if quarantine_dir.parent() != lane_dir.parent() {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "quarantine and lane directories must share the same capture queue root"
                            .to_string(),
                });
            }
            let (_q_guards, append_lock) = acquire_quarantine_lock_windows(quarantine_dir)?;
            let _l_guard = pin_windows_dir_nofollow(lane_dir, "capture queue lane directory")?;
            quarantine_and_unlink_orphan_windows_locked(
                quarantine_dir,
                lane_dir,
                lane,
                orphan_seg_id,
                &append_lock,
                observer,
            )
        }

        fn resolve_lane_min_live_seg_readonly_windows(lane_dir: &Path) -> StorageResult<u32> {
            let primary = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_FILE,
                "capture queue tail.meta",
            )?;
            let backup = read_meta_replica_windows(
                lane_dir,
                QUEUE_TAIL_META_BAK_FILE,
                "capture queue tail.meta.bak",
            )?;
            match resolve_dual_replicas(primary, backup)? {
                ResolvedReplicas::Authoritative { meta, .. } => Ok(meta.min_live_seg),
                ResolvedReplicas::BothAbsent | ResolvedReplicas::BothInvalidWithPresence => Ok(0),
            }
        }

        fn measure_lane_unconsumed_bytes_windows(
            lane_dir: &Path,
            min_live_seg: u32,
        ) -> StorageResult<u64> {
            let mut total = 0u64;
            for entry in fs::read_dir(lane_dir)
                .map_err(|e| StorageBackendError::io("read capture queue lane directory", e))?
            {
                let entry = entry
                    .map_err(|e| StorageBackendError::io("read capture queue lane entry", e))?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "non-UTF-8 entry name in capture queue lane".to_string(),
                    });
                };
                if !matches_segment_log_glob_bytes(name.as_bytes()) {
                    continue;
                }
                let entry_path = lane_dir.join(&name);
                let (_file, size, _) = open_windows_file_nofollow(
                    &entry_path,
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    OPEN_EXISTING,
                    "capture queue segment file",
                    "stat capture queue segment for occupancy",
                )?;
                // Exclude already-reclaimed segments ONLY after verifying
                // reparse-point and regular-file invariants.
                if let Some(seg_id) = parse_segment_log_file_name(name.as_bytes())
                    && seg_id < min_live_seg
                {
                    continue;
                }
                total = total
                    .checked_add(size)
                    .ok_or_else(|| StorageBackendError::QueueFull {
                        detail: "capture queue lane occupancy overflowed u64".to_string(),
                    })?;
            }
            Ok(total)
        }

        fn measure_quarantine_bytes_windows(quarantine_dir: &Path) -> StorageResult<u64> {
            let mut total = 0u64;
            for entry in fs::read_dir(quarantine_dir).map_err(|e| {
                StorageBackendError::io("read capture queue quarantine directory", e)
            })? {
                let entry = entry.map_err(|e| {
                    StorageBackendError::io("read capture queue quarantine entry", e)
                })?;
                let entry_path = entry.path();
                let (_file, size, _) = open_windows_file_nofollow(
                    &entry_path,
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    OPEN_EXISTING,
                    "capture queue quarantine entry",
                    "stat capture queue quarantine entry for occupancy",
                )?;
                total = total
                    .checked_add(size)
                    .ok_or_else(|| StorageBackendError::QueueFull {
                        detail: "capture queue quarantine occupancy overflowed u64".to_string(),
                    })?;
            }
            Ok(total)
        }

        pub(super) fn measure_occupancy_in_storage_dir_windows(
            storage_dir: &Path,
        ) -> StorageResult<CaptureQueueOccupancy> {
            let private_dir = storage_dir.join(PRIVATE_SUBDIR);
            let _private_guard =
                pin_windows_dir_nofollow(&private_dir, "capture private directory")?;
            let queue_root = private_dir.join(CAPTURE_QUEUE_SUBDIR);
            measure_occupancy_at_root_windows(&queue_root)
        }

        pub(super) fn measure_occupancy_at_root_windows(
            queue_root: &Path,
        ) -> StorageResult<CaptureQueueOccupancy> {
            let _root_guard = pin_windows_dir_nofollow(queue_root, "capture queue root directory")?;
            let priority_dir = queue_root.join(QUEUE_LANE_PRIORITY_DIR);
            let normal_dir = queue_root.join(QUEUE_LANE_NORMAL_DIR);
            let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
            let _p_guard =
                pin_windows_dir_nofollow(&priority_dir, "capture queue priority lane directory")?;
            let _n_guard =
                pin_windows_dir_nofollow(&normal_dir, "capture queue normal lane directory")?;
            let _q_guard =
                pin_windows_dir_nofollow(&quarantine_dir, "capture queue quarantine directory")?;
            let deadline = lock_deadline_for_timeout(DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT);
            let _lock = acquire_existing_lock_windows(queue_root, deadline)?;

            let priority_min = resolve_lane_min_live_seg_readonly_windows(&priority_dir)?;
            let normal_min = resolve_lane_min_live_seg_readonly_windows(&normal_dir)?;
            let priority_bytes =
                measure_lane_unconsumed_bytes_windows(&priority_dir, priority_min)?;
            let normal_bytes = measure_lane_unconsumed_bytes_windows(&normal_dir, normal_min)?;
            let quarantine_bytes = measure_quarantine_bytes_windows(&quarantine_dir)?;
            CaptureQueueOccupancy::from_parts(priority_bytes, normal_bytes, quarantine_bytes)
        }

        fn create_segment_file_windows_locked(
            lane_dir: &Path,
            lane_label: &'static str,
            next_seg: u32,
            _append_lock: &File,
            observer: &mut SegmentObserver,
        ) -> StorageResult<(File, WindowsFileIdentity)> {
            let seg_path = lane_dir.join(segment_log_file_name(next_seg));
            let (file, size, identity) = open_windows_file_nofollow(
                &seg_path,
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                CREATE_NEW,
                "capture queue segment file",
                "create capture queue segment",
            )?;
            if size != 0 {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: "newly created capture queue segment is non-empty".to_string(),
                });
            }
            observer.record_create_segment(lane_label, next_seg);
            flush_windows_dir(lane_dir)?;
            observer.record_fsync_lane_after_create_segment(lane_label, next_seg);
            Ok((file, identity))
        }

        pub(super) fn append_in_storage_dir_windows(
            storage_dir: &Path,
            lane: CaptureQueueLane,
            frame_buf: &[u8],
            options: QueueAppendOptions,
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentAppendOutcome> {
            let private_dir = storage_dir.join(PRIVATE_SUBDIR);
            let _private_guard =
                pin_windows_dir_nofollow(&private_dir, "capture private directory")?;
            let queue_root = private_dir.join(CAPTURE_QUEUE_SUBDIR);
            append_at_root_windows(&queue_root, lane, frame_buf, options, observer)
        }

        pub(super) fn append_at_root_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
            frame_buf: &[u8],
            options: QueueAppendOptions,
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentAppendOutcome> {
            let _root_guard = pin_windows_dir_nofollow(queue_root, "capture queue root directory")?;
            let lane_label = lane_dir_name(lane);
            let lane_dir = queue_root.join(lane_label);
            let _lane_guard = pin_windows_dir_nofollow(&lane_dir, "capture queue lane directory")?;
            let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
            let _q_guard =
                pin_windows_dir_nofollow(&quarantine_dir, "capture queue quarantine directory")?;

            let (append_lock, plan, pinned_seg) = acquire_lock_and_phase1_windows(
                queue_root,
                &lane_dir,
                true,
                options.effective_lock_timeout(),
                observer,
            )?;
            let _cleaned_q_tmp = clean_lane_quarantine_tmp_files_windows(&quarantine_dir, lane)?;
            let phase2_ctx = WindowsPhase2Context {
                lane,
                lane_dir: &lane_dir,
                quarantine_dir: &quarantine_dir,
                append_lock: &append_lock,
            };
            let converged = phase2_execute_plan_windows(&phase2_ctx, plan, observer)?;
            let meta = converged.meta;
            let active_seg_exists = converged.active_seg_exists;
            let recovered_frames = converged.recovered_frames;
            let pinned_seg = converged.replacement_active_seg.or(pinned_seg);

            // Step ② (a) (`ADR-RB-05` #3, `RB-08b`): pre-write cross-lane
            // capacity conservation guard AFTER Step ① recovery and BEFORE
            // writing or creating any segment for the incoming frame.
            let max_total_bytes = options.effective_max_total_bytes();
            let other_lane = opposite_lane(lane);
            let other_lane_dir = queue_root.join(lane_dir_name(other_lane));
            let _other_lane_guard =
                pin_windows_dir_nofollow(&other_lane_dir, "capture queue sibling lane directory")?;
            let target_bytes = measure_lane_unconsumed_bytes_windows(&lane_dir, meta.min_live_seg)?;
            let other_min = resolve_lane_min_live_seg_readonly_windows(&other_lane_dir)?;
            let other_bytes = measure_lane_unconsumed_bytes_windows(&other_lane_dir, other_min)?;
            let quarantine_bytes = measure_quarantine_bytes_windows(&quarantine_dir)?;
            let (priority_bytes, normal_bytes) = match lane {
                CaptureQueueLane::Priority => (target_bytes, other_bytes),
                CaptureQueueLane::Normal => (other_bytes, target_bytes),
            };
            let occupancy =
                CaptureQueueOccupancy::from_parts(priority_bytes, normal_bytes, quarantine_bytes)?;
            observer.record_capacity_checked(occupancy, frame_buf.len(), max_total_bytes);
            let _ = enforce_queue_capacity_limit(occupancy, frame_buf.len(), max_total_bytes)?;

            // Step ② (b) (`ADR-RB-05` #3, `RB-08j`) + Step ③ (`RB-08g`): check
            // whether 16 MiB rotation is needed and enforce `sealed.len() <
            // MAX_SEALED_SEGMENTS` (`256`) strictly BEFORE creating `S + 1` or
            // mutating any file for the incoming frame.
            let rotating = should_rotate_active_segment(
                meta.committed_len,
                frame_buf.len(),
                CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES,
            )?;
            enforce_rotation_sealed_cap_limit(meta.sealed.len(), rotating)?;
            let (seg_id, frame_offset, next_meta) =
                build_next_append_meta(meta, frame_buf, rotating)?;
            let mut seg_file = if rotating || !active_seg_exists {
                drop(pinned_seg);
                create_segment_file_windows_locked(
                    &lane_dir,
                    lane_label,
                    seg_id,
                    &append_lock,
                    observer,
                )?
                .0
            } else {
                // Reuse the exact pinned handle validated in Phase 1 (zero reopen TOCTOU)
                // and re-verify handle identity and length before writing.
                let pinned = pinned_seg.ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "missing pinned Windows segment handle after Phase 1".to_string(),
                })?;
                let info = file_info_by_handle(
                    pinned.file.as_raw_handle() as HANDLE,
                    "verify pinned active capture queue segment",
                )?;
                if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
                    || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0
                    || file_identity_from_info(&info) != pinned.identity
                    || file_size_from_info(&info) != frame_offset
                {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "pinned active capture queue segment identity or length diverged before append"
                            .to_string(),
                    });
                }
                pinned.file
            };
            seg_file
                .seek(SeekFrom::Start(frame_offset))
                .map_err(|e| StorageBackendError::io("seek active capture queue segment", e))?;
            seg_file
                .write_all(frame_buf)
                .map_err(|e| StorageBackendError::io("write capture queue frame", e))?;
            observer.record_segment_write_all(lane_label, seg_id, frame_offset, frame_buf.len());
            seg_file
                .sync_data()
                .map_err(|e| StorageBackendError::io("sync capture queue segment data", e))?;
            observer.record_segment_sync_data(lane_label, seg_id);
            drop(seg_file);

            observer.maybe_crash(SegmentCrashPoint::AfterSegmentSyncData)?;

            commit_dual_tail_meta_windows(&lane_dir, lane_label, &next_meta, observer)?;

            drop(append_lock);
            observer.record_release_lock();

            Ok(SegmentAppendOutcome {
                lane,
                seg_id,
                frame_offset,
                frame_len: frame_buf.len(),
                meta: next_meta,
                recovered_frames,
            })
        }

        pub(super) fn recover_in_storage_dir_windows(
            storage_dir: &Path,
            lane: CaptureQueueLane,
            observer: &mut SegmentObserver,
        ) -> StorageResult<LaneRecoveryReport> {
            let private_dir = storage_dir.join(PRIVATE_SUBDIR);
            let _private_guard =
                pin_windows_dir_nofollow(&private_dir, "capture private directory")?;
            let queue_root = private_dir.join(CAPTURE_QUEUE_SUBDIR);
            recover_at_root_windows(&queue_root, lane, observer)
        }

        pub(super) fn recover_at_root_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
            observer: &mut SegmentObserver,
        ) -> StorageResult<LaneRecoveryReport> {
            let _root_guard = pin_windows_dir_nofollow(queue_root, "capture queue root directory")?;
            let lane_label = lane_dir_name(lane);
            let lane_dir = queue_root.join(lane_label);
            let _lane_guard = pin_windows_dir_nofollow(&lane_dir, "capture queue lane directory")?;
            let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
            let _q_guard =
                pin_windows_dir_nofollow(&quarantine_dir, "capture queue quarantine directory")?;

            let (append_lock, plan, pinned) = acquire_lock_and_phase1_windows(
                queue_root,
                &lane_dir,
                false,
                DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT,
                observer,
            )?;
            drop(pinned);
            let cleaned_quarantine_tmp_files =
                clean_lane_quarantine_tmp_files_windows(&quarantine_dir, lane)?;
            let phase2_ctx = WindowsPhase2Context {
                lane,
                lane_dir: &lane_dir,
                quarantine_dir: &quarantine_dir,
                append_lock: &append_lock,
            };
            let converged = phase2_execute_plan_windows(&phase2_ctx, plan, observer)?;
            if !converged.lane_dir_synced {
                flush_windows_dir(&lane_dir)?;
                observer.record_fsync_lane_on_recovery(lane_label);
            }

            drop(append_lock);
            observer.record_release_lock();

            Ok(LaneRecoveryReport {
                lane,
                meta: converged.meta,
                recovered_frames: converged.recovered_frames,
                recovered_bytes: converged.recovered_bytes,
                mirrored_replica: converged.mirrored_replica,
                cleaned_tmp_files: converged.cleaned_tmp_files,
                cleaned_quarantine_tmp_files,
                reclaimed_orphans_removed: converged.reclaimed_orphans_removed,
                quarantined_artifacts: converged.quarantined_artifacts,
            })
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        pub(super) fn reclaim_in_storage_dir_windows(
            storage_dir: &Path,
            lane: CaptureQueueLane,
            seg_id: u32,
            highwater_seg: u32,
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentReclaimOutcome> {
            let private_dir = storage_dir.join(PRIVATE_SUBDIR);
            let _private_guard =
                pin_windows_dir_nofollow(&private_dir, "capture private directory")?;
            let queue_root = private_dir.join(CAPTURE_QUEUE_SUBDIR);
            reclaim_at_root_windows(&queue_root, lane, seg_id, highwater_seg, observer)
        }

        #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
        pub(super) fn reclaim_at_root_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
            seg_id: u32,
            highwater_seg: u32,
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentReclaimOutcome> {
            let _root_guard = pin_windows_dir_nofollow(queue_root, "capture queue root directory")?;
            let lane_label = lane_dir_name(lane);
            let lane_dir = queue_root.join(lane_label);
            let _lane_guard = pin_windows_dir_nofollow(&lane_dir, "capture queue lane directory")?;
            let quarantine_dir = queue_root.join(QUEUE_QUARANTINE_DIR);
            let _q_guard =
                pin_windows_dir_nofollow(&quarantine_dir, "capture queue quarantine directory")?;

            let (append_lock, plan, pinned) = acquire_lock_and_phase1_windows(
                queue_root,
                &lane_dir,
                false,
                DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT,
                observer,
            )?;
            drop(pinned);

            // Strictly read-only pre-validation BEFORE any Phase 2 metadata
            // repair or `.tmp` cleanup: reject unconsumed/non-oldest/active
            // segments and reject missing/reparse/dir/foreign-owned target
            // files with zero disk mutation.
            validate_reclaim_plan_readonly(&plan, seg_id, highwater_seg)?;
            let seg_path = lane_dir.join(segment_log_file_name(seg_id));
            preverify_reclaim_target_windows(&seg_path, observer)?;

            let _cleaned_q_tmp = clean_lane_quarantine_tmp_files_windows(&quarantine_dir, lane)?;
            let phase2_ctx = WindowsPhase2Context {
                lane,
                lane_dir: &lane_dir,
                quarantine_dir: &quarantine_dir,
                append_lock: &append_lock,
            };
            let converged = phase2_execute_plan_windows(&phase2_ctx, plan, observer)?;
            let next_meta = build_reclaimed_tail_meta(&converged.meta, seg_id, highwater_seg)?;

            // Step ①: atomically commit dual-replica `LTM2` BEFORE deleting file.
            commit_dual_tail_meta_windows(&lane_dir, lane_label, &next_meta, observer)?;

            observer.maybe_crash(SegmentCrashPoint::AfterReclaimMetaBeforeUnlink)?;

            // Step ②: handle-pinned non-reparse, owner-verified deletion + flush lane directory.
            let file_removed = match open_windows_file_nofollow(
                &seg_path,
                GENERIC_READ | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                OPEN_EXISTING,
                "reclaimed capture queue segment",
                "open reclaimed capture queue segment",
            ) {
                Ok((file, _, _)) => {
                    verify_windows_reclaim_target_owner(&file, observer)?;
                    delete_windows_file_by_handle(file, "delete reclaimed capture queue segment")?;
                    observer.record_reclaim_segment_unlink(lane_label, seg_id);
                    flush_windows_dir(&lane_dir)?;
                    observer.record_fsync_lane_after_reclaim(lane_label, seg_id);
                    true
                }
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => false,
                Err(err) => return Err(err),
            };

            drop(append_lock);
            observer.record_release_lock();

            Ok(SegmentReclaimOutcome {
                lane,
                reclaimed_seg_id: seg_id,
                meta: next_meta,
                file_removed,
            })
        }
    }

    #[cfg(windows)]
    pub(super) fn read_converged_lane_tail_meta_non_unix(
        lane_dir: &Path,
    ) -> StorageResult<LaneTailMeta> {
        win_impl::read_converged_lane_tail_meta_windows(lane_dir)
    }

    #[cfg(windows)]
    pub(super) fn publish_quarantine_durable_non_unix(
        quarantine_dir: &Path,
        id: &str,
        bytes: &[u8],
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        win_impl::publish_quarantine_durable_windows(quarantine_dir, id, bytes, observer)
    }

    #[cfg(windows)]
    pub(super) fn quarantine_and_truncate_segment_non_unix(
        quarantine_dir: &Path,
        lane_dir: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        valid_end: u64,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        win_impl::quarantine_and_truncate_segment_windows(
            quarantine_dir,
            lane_dir,
            lane,
            seg_id,
            valid_end,
            observer,
        )
    }

    #[cfg(windows)]
    pub(super) fn quarantine_and_unlink_orphan_non_unix(
        quarantine_dir: &Path,
        lane_dir: &Path,
        lane: CaptureQueueLane,
        orphan_seg_id: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<QuarantinePublishReport> {
        win_impl::quarantine_and_unlink_orphan_windows(
            quarantine_dir,
            lane_dir,
            lane,
            orphan_seg_id,
            observer,
        )
    }

    #[cfg(windows)]
    pub(super) fn measure_occupancy_in_storage_dir_non_unix(
        storage_dir: &Path,
    ) -> StorageResult<CaptureQueueOccupancy> {
        win_impl::measure_occupancy_in_storage_dir_windows(storage_dir)
    }

    #[cfg(windows)]
    pub(super) fn measure_occupancy_at_root_non_unix(
        queue_root: &Path,
    ) -> StorageResult<CaptureQueueOccupancy> {
        win_impl::measure_occupancy_at_root_windows(queue_root)
    }

    #[cfg(windows)]
    pub(super) fn append_in_storage_dir_non_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        options: QueueAppendOptions,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        win_impl::append_in_storage_dir_windows(storage_dir, lane, frame_buf, options, observer)
    }

    #[cfg(windows)]
    pub(super) fn append_at_root_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        options: QueueAppendOptions,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        win_impl::append_at_root_windows(queue_root, lane, frame_buf, options, observer)
    }

    #[cfg(windows)]
    pub(super) fn recover_in_storage_dir_non_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        win_impl::recover_in_storage_dir_windows(storage_dir, lane, observer)
    }

    #[cfg(windows)]
    pub(super) fn recover_at_root_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        win_impl::recover_at_root_windows(queue_root, lane, observer)
    }

    #[cfg(windows)]
    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_in_storage_dir_non_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        highwater_seg: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        win_impl::reclaim_in_storage_dir_windows(storage_dir, lane, seg_id, highwater_seg, observer)
    }

    #[cfg(windows)]
    #[allow(dead_code)] // Consumed by `storage::queue::writer` starting in RB-09.
    pub(super) fn reclaim_at_root_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        seg_id: u32,
        highwater_seg: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentReclaimOutcome> {
        win_impl::reclaim_at_root_windows(queue_root, lane, seg_id, highwater_seg, observer)
    }
}

// ============================================================================
// Tests (`RB-08` AC1–AC6)
// ============================================================================

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        fs,
        process::Command,
    };

    use tempfile::tempdir;

    use super::*;
    use crate::storage::queue::{
        DecodedFrame, FRAME_BODY_FIXED_OVERHEAD, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
        FRAME_HEADER_LEN, SealedSegmentMeta, crc32c_checksum, init_capture_queue,
        tail_meta_bak_path, tail_meta_path,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum SnapshotEntry {
        Dir { mode: u32 },
        File { mode: u32, bytes: Vec<u8> },
        Symlink { target: PathBuf },
    }

    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, SnapshotEntry>) {
            let mut entries: Vec<_> = fs::read_dir(dir)
                .expect("read_dir for snapshot")
                .map(|e| e.expect("dir entry for snapshot"))
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                let rel = p.strip_prefix(base).expect("strip base").to_path_buf();
                let md = fs::symlink_metadata(&p).expect("symlink_metadata for snapshot");
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt as _;
                    md.permissions().mode() & 0o7777
                };
                #[cfg(not(unix))]
                let mode = 0u32;
                if md.file_type().is_symlink() {
                    out.insert(
                        rel,
                        SnapshotEntry::Symlink {
                            target: fs::read_link(&p).expect("read_link for snapshot"),
                        },
                    );
                } else if md.is_dir() {
                    out.insert(rel, SnapshotEntry::Dir { mode });
                    walk(base, &p, out);
                } else {
                    out.insert(
                        rel,
                        SnapshotEntry::File {
                            mode,
                            bytes: fs::read(&p).expect("read file for snapshot"),
                        },
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    const CHILD_DIR_ENV: &str = "LIBRA_RB08_PRODUCER_CHILD_DIR";
    const CHILD_ID_ENV: &str = "LIBRA_RB08_PRODUCER_ID";
    const CHILD_COUNT_ENV: &str = "LIBRA_RB08_PRODUCER_COUNT";
    const CHILD_READY_DIR_ENV: &str = "LIBRA_RB08_READY_DIR";
    const CHILD_GO_FILE_ENV: &str = "LIBRA_RB08_GO_FILE";

    /// Compute exact total encoded frame length in `[64, 65536]` (`64 B – 64 KiB`)
    /// for `(worker_id, seq)` so every worker exercises both the exact 64 B lower
    /// bound, exact 64 KiB upper bound, and variable intermediate lengths.
    fn deterministic_frame_total_len(worker_id: usize, seq: usize) -> usize {
        if seq == 0 {
            64
        } else if seq == 99 {
            64 * 1024
        } else {
            match seq % 19 {
                0 => 16 * 1024 + ((worker_id * 137 + seq * 53) % 4096),
                7 => 4096 + ((worker_id * 97 + seq * 31) % 2048),
                _ => 64 + ((worker_id * 251 + seq * 109) % 1024),
            }
        }
    }

    /// Build a deterministic frame whose total encoded wire length equals
    /// `deterministic_frame_total_len(worker_id, seq)` exactly.
    fn build_deterministic_worker_frame(
        worker_id: usize,
        seq: usize,
    ) -> (String, Vec<u8>, Vec<u8>) {
        let dedup_key = format!("rb08:w{worker_id:02}:s{seq:03}");
        let target_total = deterministic_frame_total_len(worker_id, seq);
        let overhead = FRAME_HEADER_LEN + FRAME_BODY_FIXED_OVERHEAD + dedup_key.len();
        assert!(
            target_total >= overhead,
            "target_total={target_total} must cover overhead={overhead}"
        );
        let payload_len = target_total - overhead;
        let mut payload = vec![0u8; payload_len];
        let mut state: u64 =
            ((worker_id as u64 + 1) << 32) ^ (seq as u64 + 1) ^ 0x9E37_79B9_7F4A_7C15;
        for byte in &mut payload {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = (state & 0xFF) as u8;
        }
        let encoded =
            encode_frame(FRAME_FLAG_NONE, &dedup_key, &payload).expect("encode worker frame");
        assert_eq!(encoded.len(), target_total);
        (dedup_key, payload, encoded)
    }

    fn run_multiprocess_child_worker() {
        let storage_dir =
            Path::new(&std::env::var_os(CHILD_DIR_ENV).expect("child storage dir env must be set"))
                .to_path_buf();
        let worker_id: usize = std::env::var(CHILD_ID_ENV)
            .expect("child worker id env")
            .parse()
            .expect("parse worker id");
        let count: usize = std::env::var(CHILD_COUNT_ENV)
            .expect("child count env")
            .parse()
            .expect("parse count");
        let ready_dir = Path::new(
            &std::env::var_os(CHILD_READY_DIR_ENV).expect("child ready dir env must be set"),
        )
        .to_path_buf();
        let go_file =
            Path::new(&std::env::var_os(CHILD_GO_FILE_ENV).expect("child go file env must be set"))
                .to_path_buf();

        // Signal parent that this child process is alive and ready to contend.
        fs::write(ready_dir.join(format!("ready-{worker_id:02}")), b"ok")
            .expect("write ready marker");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !go_file.exists() {
            assert!(
                Instant::now() < deadline,
                "worker {worker_id} timed out waiting for go barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        for seq in 0..count {
            let (_dedup_key, _payload, encoded) = build_deterministic_worker_frame(worker_id, seq);
            let outcome = append_encoded_frame(&storage_dir, CaptureQueueLane::Normal, &encoded)
                .expect("child append_encoded_frame must succeed");
            assert_eq!(outcome.frame_len, encoded.len());
            assert_eq!(outcome.lane, CaptureQueueLane::Normal);
        }
        println!("RB08_CHILD_OK:{worker_id:02}");
    }

    fn decode_all_segment_frames(lane_dir: &Path, meta: &LaneTailMeta) -> Vec<DecodedFrame> {
        let mut all_frames = Vec::new();
        for seg_id in meta.min_live_seg..=meta.active_seg {
            let seg_path = lane_dir.join(segment_log_file_name(seg_id));
            let file = File::open(&seg_path).expect("open segment file for verification");
            let mut decoder =
                FrameStreamDecoder::from_seekable(file).expect("construct seekable decoder");
            loop {
                match decoder.next_frame().expect("every frame must pass CRC32C") {
                    FrameStreamItem::Frame { frame, .. } => all_frames.push(frame),
                    FrameStreamItem::CleanEof { offset } => {
                        if seg_id == meta.active_seg {
                            assert_eq!(
                                offset, meta.committed_len,
                                "active segment CleanEof must match committed_len"
                            );
                        }
                        break;
                    }
                }
            }
        }
        all_frames
    }

    #[test]
    fn multiprocess_32_producers_zero_interleave_and_dual_tail_meta() {
        if std::env::var_os(CHILD_DIR_ENV).is_some() {
            run_multiprocess_child_worker();
            return;
        }

        // ------------------------------------------------------------------
        // Part A: AC2, AC3, AC4 ordered durability & dual-replica convergence
        // ------------------------------------------------------------------
        let ordered_repo = tempdir().expect("create ordered durability tempdir");
        let init = init_capture_queue(ordered_repo.path()).expect("init capture queue");
        let frame1 = encode_frame(FRAME_FLAG_PRIORITY, "rb08:order:1", b"alpha-payload")
            .expect("encode frame1");

        let mut observer = SegmentObserver::new();
        let outcome1 = append_validated_frame_in_storage_dir(
            ordered_repo.path(),
            CaptureQueueLane::Priority,
            &frame1,
            &mut observer,
        )
        .expect("append frame1");
        assert_eq!(outcome1.seg_id, 0);
        assert_eq!(outcome1.frame_offset, 0);
        assert_eq!(outcome1.frame_len, frame1.len());
        assert_eq!(outcome1.meta.committed_len, frame1.len() as u64);
        assert_eq!(outcome1.meta.prefix_crc32c, crc32c_checksum(&frame1));

        // Assert exact strict ordering of AC2, AC3, AC4 before lock release:
        // AcquireAppendLock < CreateSegment < FsyncLaneDirAfterCreateSegment
        // < SegmentWriteAll < SegmentSyncData
        // < MetaTmpWriteAndSync(tail.meta.tmp) < MetaRename(tail.meta.tmp -> tail.meta)
        // < FsyncLaneDirAfterMeta(tail.meta)
        // < MetaTmpWriteAndSync(tail.meta.bak.tmp) < MetaRename(tail.meta.bak.tmp -> tail.meta.bak)
        // < FsyncLaneDirAfterMeta(tail.meta.bak)
        // < ReleaseAppendLock
        assert_eq!(
            observer.events,
            vec![
                SegmentOpEvent::AcquireAppendLock,
                SegmentOpEvent::CapacityChecked {
                    priority_bytes: 0,
                    normal_bytes: 0,
                    quarantine_bytes: 0,
                    incoming_bytes: frame1.len(),
                    max_total_bytes: CAPTURE_QUEUE_MAX_TOTAL_BYTES,
                },
                SegmentOpEvent::CreateSegment {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 0,
                },
                SegmentOpEvent::FsyncLaneDirAfterCreateSegment {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 0,
                },
                SegmentOpEvent::SegmentWriteAll {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 0,
                    offset: 0,
                    len: frame1.len(),
                },
                SegmentOpEvent::SegmentSyncData {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 0,
                },
                SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::MetaRename {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::MetaRename {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::ReleaseAppendLock,
            ],
            "enqueue must strictly execute write_all -> sync_data -> tail.meta(tmp/rename/fsync_dir) -> tail.meta.bak(tmp/rename/fsync_dir) before releasing append.lock"
        );

        let pri_bytes = fs::read(tail_meta_path(&init.paths.priority)).expect("read tail.meta");
        let bak_bytes =
            fs::read(tail_meta_bak_path(&init.paths.priority)).expect("read tail.meta.bak");
        assert_eq!(pri_bytes, bak_bytes);
        assert_eq!(
            LaneTailMeta::decode(&pri_bytes).expect("decode tail.meta"),
            outcome1.meta
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for p in [
                tail_meta_path(&init.paths.priority),
                tail_meta_bak_path(&init.paths.priority),
                init.paths.priority.join(segment_log_file_name(0)),
            ] {
                let mode = fs::symlink_metadata(&p)
                    .expect("stat file")
                    .permissions()
                    .mode()
                    & 0o7777;
                assert_eq!(mode, QUEUE_FILE_MODE, "expected 0600 on {}", p.display());
            }
        }

        // AC4: Verify strict total-order convergence `(active_seg, committed_len, min_live_seg)`.
        let older_meta = LaneTailMeta::initial_empty();
        let newer_meta = outcome1.meta.clone();
        // Backup ahead, primary behind -> converges to backup and mirrors primary.
        fs::write(
            tail_meta_path(&init.paths.priority),
            older_meta.encode().expect("encode older"),
        )
        .expect("regress primary replica");
        let converged_ro =
            read_converged_lane_tail_meta(&init.paths.priority).expect("read converged");
        assert_eq!(converged_ro, newer_meta);
        let rec = recover_lane(ordered_repo.path(), CaptureQueueLane::Priority)
            .expect("recover lagging primary");
        assert!(rec.mirrored_replica);
        assert_eq!(rec.meta, newer_meta);
        assert_eq!(
            fs::read(tail_meta_path(&init.paths.priority)).expect("read mirrored primary"),
            fs::read(tail_meta_bak_path(&init.paths.priority)).expect("read backup")
        );

        // Verify all 3 components of ordering key `(active_seg, committed_len, min_live_seg)`
        // and fail-closed disagreement when ordering keys match with differing checksums.
        let m_seg0_reclaimed = LaneTailMeta {
            min_live_seg: 1,
            active_seg: 2,
            committed_len: 100,
            prefix_crc32c: 0x1111_2222,
            sealed: vec![SealedSegmentMeta {
                seg_id: 1,
                sealed_len: 500,
                sealed_crc32c: 0xAAAA_BBBB,
            }],
        };
        let m_seg0_unreclaimed = LaneTailMeta {
            min_live_seg: 0,
            active_seg: 2,
            committed_len: 100,
            prefix_crc32c: 0x1111_2222,
            sealed: vec![
                SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: 400,
                    sealed_crc32c: 0xCCCC_DDDD,
                },
                SealedSegmentMeta {
                    seg_id: 1,
                    sealed_len: 500,
                    sealed_crc32c: 0xAAAA_BBBB,
                },
            ],
        };
        assert!(m_seg0_reclaimed.ordering_key() > m_seg0_unreclaimed.ordering_key());
        assert_eq!(
            resolve_dual_replicas(
                ReplicaState::Valid(m_seg0_unreclaimed.clone()),
                ReplicaState::Valid(m_seg0_reclaimed.clone()),
            )
            .expect("resolve min_live_seg tiebreak"),
            ResolvedReplicas::Authoritative {
                meta: m_seg0_reclaimed.clone(),
                mirror_sync: MirrorSync::SyncPrimary,
            }
        );

        let mut conflicting = m_seg0_reclaimed.clone();
        conflicting.prefix_crc32c ^= 1;
        let conflict_err = resolve_dual_replicas(
            ReplicaState::Valid(m_seg0_reclaimed),
            ReplicaState::Valid(conflicting),
        )
        .expect_err("same ordering key with different prefix_crc32c must fail closed");
        assert!(matches!(
            conflict_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(conflict_err.stable_code_str(), "LBR-IO-001");

        // ------------------------------------------------------------------
        // Part B: AC1 — 32 concurrent child processes x 100 frames (64B–64KiB)
        // ------------------------------------------------------------------
        const WORKERS: usize = 32;
        const FRAMES_PER_WORKER: usize = 100;
        const TOTAL_FRAMES: usize = WORKERS * FRAMES_PER_WORKER;

        let mp_repo = tempdir().expect("create multiprocess repo tempdir");
        let mp_init = init_capture_queue(mp_repo.path()).expect("init multiprocess capture queue");
        let sync_dir = tempdir().expect("create barrier tempdir");
        let ready_dir = sync_dir.path().join("ready");
        fs::create_dir_all(&ready_dir).expect("create ready dir");
        let go_file = sync_dir.path().join("go");

        let exe = std::env::current_exe().expect("locate current test executable");
        let mut children = Vec::with_capacity(WORKERS);
        for worker_id in 0..WORKERS {
            let child = Command::new(&exe)
                .arg("--exact")
                .arg("storage::queue::segment::tests::multiprocess_32_producers_zero_interleave_and_dual_tail_meta")
                .arg("--nocapture")
                .env(CHILD_DIR_ENV, mp_repo.path())
                .env(CHILD_ID_ENV, worker_id.to_string())
                .env(CHILD_COUNT_ENV, FRAMES_PER_WORKER.to_string())
                .env(CHILD_READY_DIR_ENV, &ready_dir)
                .env(CHILD_GO_FILE_ENV, &go_file)
                .env("LIBRA_TEST", "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn producer child process");
            children.push((worker_id, child));
        }

        // Wait until all 32 child processes have started and reached the barrier.
        let ready_deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let ready_count = (0..WORKERS)
                .filter(|id| ready_dir.join(format!("ready-{id:02}")).exists())
                .count();
            if ready_count == WORKERS {
                break;
            }
            assert!(
                Instant::now() < ready_deadline,
                "only {ready_count}/{WORKERS} child processes reached start barrier"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Release all 32 child processes simultaneously.
        fs::write(&go_file, b"go").expect("release go barrier");

        for (worker_id, child) in children {
            let output = child.wait_with_output().expect("wait on child process");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let expected_marker = format!("RB08_CHILD_OK:{worker_id:02}");
            assert!(
                output.status.success() && stdout.contains(&expected_marker),
                "worker {worker_id} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }

        // Verify exact dual-replica equality & segment prefix CRC32C on disk.
        let normal_dir = &mp_init.paths.normal;
        let final_primary_raw = fs::read(tail_meta_path(normal_dir)).expect("read final tail.meta");
        let final_backup_raw =
            fs::read(tail_meta_bak_path(normal_dir)).expect("read final tail.meta.bak");
        assert_eq!(
            final_primary_raw, final_backup_raw,
            "tail.meta and tail.meta.bak must match byte-for-byte after 32-process contention"
        );
        let final_meta =
            LaneTailMeta::decode(&final_primary_raw).expect("decode final converged LTM2");
        assert_eq!(final_meta.min_live_seg, 0);
        assert_eq!(final_meta.active_seg, 0);

        let seg0_raw =
            fs::read(normal_dir.join(segment_log_file_name(0))).expect("read events-00000000.log");
        assert_eq!(seg0_raw.len() as u64, final_meta.committed_len);
        assert_eq!(crc32c_checksum(&seg0_raw), final_meta.prefix_crc32c);

        // Verify zero leftover `.tmp` files in `normal/`.
        for entry in fs::read_dir(normal_dir).expect("read normal dir") {
            let name = entry.expect("dir entry").file_name();
            assert!(
                !is_lane_meta_temp_name(name.to_string_lossy().as_bytes()),
                "stale temp file left behind: {name:?}"
            );
        }

        // Verify all 3,200 frames decode with 100% CRC32C pass, exact payloads,
        // exact 64 B – 64 KiB length coverage, and zero interleaving or duplicates.
        let decoded = decode_all_segment_frames(normal_dir, &final_meta);
        assert_eq!(
            decoded.len(),
            TOTAL_FRAMES,
            "expected all {TOTAL_FRAMES} frames across 32 processes"
        );

        let mut seen = HashSet::with_capacity(TOTAL_FRAMES);
        let mut min_wire_len = usize::MAX;
        let mut max_wire_len = 0usize;
        for frame in &decoded {
            assert_eq!(frame.flags, FRAME_FLAG_NONE);
            let wire_len = FRAME_HEADER_LEN
                + FRAME_BODY_FIXED_OVERHEAD
                + frame.dedup_key.len()
                + frame.payload.len();
            min_wire_len = min_wire_len.min(wire_len);
            max_wire_len = max_wire_len.max(wire_len);

            let parts: Vec<&str> = frame.dedup_key.split(':').collect();
            assert_eq!(parts.len(), 3, "unexpected dedup_key: {}", frame.dedup_key);
            let worker_id: usize = parts[1]
                .strip_prefix('w')
                .expect("worker prefix")
                .parse()
                .expect("parse worker_id");
            let seq: usize = parts[2]
                .strip_prefix('s')
                .expect("seq prefix")
                .parse()
                .expect("parse seq");
            assert!(
                seen.insert((worker_id, seq)),
                "duplicate frame ({worker_id}, {seq})"
            );
            let (_, expected_payload, _) = build_deterministic_worker_frame(worker_id, seq);
            assert_eq!(
                frame.payload, expected_payload,
                "payload mismatch for ({worker_id}, {seq})"
            );
        }
        assert_eq!(seen.len(), TOTAL_FRAMES);
        assert_eq!(min_wire_len, 64, "must cover exact 64 B lower bound");
        assert_eq!(
            max_wire_len,
            64 * 1024,
            "must cover exact 64 KiB upper bound"
        );
    }

    #[test]
    fn case_c_same_segment_crash_retains_synced_frames() {
        // ------------------------------------------------------------------
        // Scenario 1 (AC5 + AC6): Fresh empty lane first-frame crash after
        // `seg.sync_data()` before `tail.meta` commit (`committed_len == 0`).
        // ------------------------------------------------------------------
        let fresh_repo = tempdir().expect("create fresh lane crash repo");
        let fresh_init = init_capture_queue(fresh_repo.path()).expect("init fresh capture queue");
        let first_frame = encode_frame(
            FRAME_FLAG_PRIORITY,
            "rb08:case-c:fresh-first",
            b"first-frame-synced-before-meta-commit",
        )
        .expect("encode first_frame");

        let mut crash_obs = SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
        let crash_err = append_validated_frame_in_storage_dir(
            fresh_repo.path(),
            CaptureQueueLane::Priority,
            &first_frame,
            &mut crash_obs,
        )
        .expect_err("simulated crash after seg.sync_data() must fail");
        assert_eq!(crash_err.stable_code_str(), "LBR-IO-001");

        // Confirm on-disk state right after crash: both replicas still at initial
        // 34B `(0, 0, 0, 0, [])`, while `events-00000000.log` has exact `first_frame`.
        let pre_primary = LaneTailMeta::decode(
            &fs::read(tail_meta_path(&fresh_init.paths.priority)).expect("read pre tail.meta"),
        )
        .expect("decode pre tail.meta");
        let pre_backup = LaneTailMeta::decode(
            &fs::read(tail_meta_bak_path(&fresh_init.paths.priority))
                .expect("read pre tail.meta.bak"),
        )
        .expect("decode pre tail.meta.bak");
        assert!(pre_primary.is_initial_empty());
        assert!(pre_backup.is_initial_empty());
        let seg0_after_crash = fs::read(fresh_init.paths.priority.join(segment_log_file_name(0)))
            .expect("read crashed seg0");
        assert_eq!(seg0_after_crash, first_frame);

        // Recover via explicit `recover_lane` and verify dual-replica atomic advance.
        let mut rec_obs = SegmentObserver::new();
        let report = recover_lane_in_storage_dir(
            fresh_repo.path(),
            CaptureQueueLane::Priority,
            &mut rec_obs,
        )
        .expect("Case C recovery on fresh first-frame crash must succeed");
        assert_eq!(report.recovered_frames, 1);
        assert_eq!(report.recovered_bytes, first_frame.len() as u64);
        assert_eq!(report.meta.active_seg, 0);
        assert_eq!(report.meta.committed_len, first_frame.len() as u64);
        assert_eq!(report.meta.prefix_crc32c, crc32c_checksum(&first_frame));

        // Verify Case C recovery committed BOTH `tail.meta` and `tail.meta.bak`
        // via atomic `.tmp -> rename -> fsync_dir` before releasing `append.lock`.
        assert!(
            rec_obs
                .events
                .contains(&SegmentOpEvent::CaseCRecoverFrames {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 0,
                    from_offset: 0,
                    to_offset: first_frame.len() as u64,
                    frame_count: 1,
                })
        );
        let pos_primary_fsync = rec_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    target_name: QUEUE_TAIL_META_FILE,
                }
            })
            .expect("primary fsync recorded");
        let pos_backup_fsync = rec_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("backup fsync recorded");
        let pos_unlock = rec_obs
            .events
            .iter()
            .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
            .expect("unlock recorded");
        assert!(pos_primary_fsync < pos_backup_fsync && pos_backup_fsync < pos_unlock);

        let post_primary_raw =
            fs::read(tail_meta_path(&fresh_init.paths.priority)).expect("read post tail.meta");
        let post_backup_raw = fs::read(tail_meta_bak_path(&fresh_init.paths.priority))
            .expect("read post tail.meta.bak");
        assert_eq!(post_primary_raw, post_backup_raw);
        assert_eq!(
            LaneTailMeta::decode(&post_primary_raw).expect("decode post LTM2"),
            report.meta
        );

        let frames_fresh = decode_all_segment_frames(&fresh_init.paths.priority, &report.meta);
        assert_eq!(frames_fresh.len(), 1);
        assert_eq!(frames_fresh[0].dedup_key, "rb08:case-c:fresh-first");
        assert_eq!(
            frames_fresh[0].payload,
            b"first-frame-synced-before-meta-commit"
        );

        // ------------------------------------------------------------------
        // Scenario 2 (AC5 + AC6): Non-empty committed prefix + multiple synced
        // uncommitted frames retained transparently during next producer append.
        // ------------------------------------------------------------------
        let committed_a = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:a",
            b"committed-prefix-frame-a",
        )
        .expect("encode a");
        let committed_b = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:b",
            b"committed-prefix-frame-b",
        )
        .expect("encode b");
        let synced_c = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:c",
            b"synced-uncommitted-frame-c",
        )
        .expect("encode c");
        let synced_d = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:d",
            b"synced-uncommitted-frame-d",
        )
        .expect("encode d");
        let next_e = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:e",
            b"subsequent-producer-frame-e",
        )
        .expect("encode e");

        let out_a = append_encoded_frame(fresh_repo.path(), CaptureQueueLane::Normal, &committed_a)
            .expect("append a");
        let out_b = append_encoded_frame(fresh_repo.path(), CaptureQueueLane::Normal, &committed_b)
            .expect("append b");
        assert_eq!(out_a.frame_offset, 0);
        assert_eq!(
            out_b.meta.committed_len,
            (committed_a.len() + committed_b.len()) as u64
        );

        // Crash right after `seg.sync_data()` on frame C.
        let mut crash_c = SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
        let _ = append_validated_frame_in_storage_dir(
            fresh_repo.path(),
            CaptureQueueLane::Normal,
            &synced_c,
            &mut crash_c,
        )
        .expect_err("simulated crash after syncing frame C");

        // Also append a second complete synced uncommitted frame D directly at
        // the uncommitted tail to prove multi-frame forward scan retention.
        let seg_normal = fresh_init.paths.normal.join(segment_log_file_name(0));
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg_normal)
                .expect("open normal seg0");
            f.write_all(&synced_d).expect("write synced_d");
            f.sync_data().expect("sync synced_d");
        }

        // Next normal producer appends frame E -> must first recover C + D into
        // both LTM2 replicas, then append E cleanly after D.
        let out_e = append_encoded_frame(fresh_repo.path(), CaptureQueueLane::Normal, &next_e)
            .expect("append E after Case C recovery must succeed");
        assert_eq!(out_e.recovered_frames, 2, "must retain both C and D");
        let expected_total_len = (committed_a.len()
            + committed_b.len()
            + synced_c.len()
            + synced_d.len()
            + next_e.len()) as u64;
        assert_eq!(out_e.meta.committed_len, expected_total_len);
        let seg_normal_bytes = fs::read(&seg_normal).expect("read final normal seg0");
        assert_eq!(seg_normal_bytes.len() as u64, expected_total_len);
        assert_eq!(out_e.meta.prefix_crc32c, crc32c_checksum(&seg_normal_bytes));

        let norm_pri = fs::read(tail_meta_path(&fresh_init.paths.normal)).expect("read normal pri");
        let norm_bak =
            fs::read(tail_meta_bak_path(&fresh_init.paths.normal)).expect("read normal bak");
        assert_eq!(norm_pri, norm_bak);
        assert_eq!(
            LaneTailMeta::decode(&norm_pri).expect("decode normal LTM2"),
            out_e.meta
        );

        let decoded_normal = decode_all_segment_frames(&fresh_init.paths.normal, &out_e.meta);
        let keys: Vec<&str> = decoded_normal
            .iter()
            .map(|f| f.dedup_key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "rb08:case-c:a",
                "rb08:case-c:b",
                "rb08:case-c:c",
                "rb08:case-c:d",
                "rb08:case-c:e",
            ]
        );

        // ------------------------------------------------------------------
        // Scenario 3: Crash after `tail.meta` commit & before `tail.meta.bak`
        // rename (leaving stale `tail.meta.bak.tmp` on disk) -> converges
        // cleanly and removes stale `.tmp`.
        // ------------------------------------------------------------------
        let frame_f = encode_frame(
            FRAME_FLAG_NONE,
            "rb08:case-c:f",
            b"backup-tmp-crash-frame-f",
        )
        .expect("encode f");
        let mut bak_crash = SegmentObserver::with_crash(SegmentCrashPoint::BeforeBackupMetaRename);
        let _ = append_validated_frame_in_storage_dir(
            fresh_repo.path(),
            CaptureQueueLane::Normal,
            &frame_f,
            &mut bak_crash,
        )
        .expect_err("simulated crash before tail.meta.bak rename");
        assert!(
            fresh_init
                .paths
                .normal
                .join(QUEUE_TAIL_META_BAK_TMP_FILE)
                .exists(),
            "simulated crash must leave tail.meta.bak.tmp on disk"
        );
        let pri_ahead = LaneTailMeta::decode(
            &fs::read(tail_meta_path(&fresh_init.paths.normal)).expect("read ahead primary"),
        )
        .expect("decode ahead primary");
        let bak_behind = LaneTailMeta::decode(
            &fs::read(tail_meta_bak_path(&fresh_init.paths.normal)).expect("read behind backup"),
        )
        .expect("decode behind backup");
        assert!(pri_ahead.ordering_key() > bak_behind.ordering_key());

        let bak_rec =
            recover_lane(fresh_repo.path(), CaptureQueueLane::Normal).expect("recover backup lag");
        assert!(bak_rec.mirrored_replica);
        assert_eq!(bak_rec.cleaned_tmp_files, 1);
        assert_eq!(bak_rec.meta, pri_ahead);
        assert!(
            !fresh_init
                .paths
                .normal
                .join(QUEUE_TAIL_META_BAK_TMP_FILE)
                .exists(),
            "Phase 2 must clean stale tail.meta.bak.tmp"
        );
        assert_eq!(
            fs::read(tail_meta_path(&fresh_init.paths.normal)).expect("read converged pri"),
            fs::read(tail_meta_bak_path(&fresh_init.paths.normal)).expect("read converged bak")
        );

        // ------------------------------------------------------------------
        // Scenario 4: Corrupted `[0, committed_len)` prefix BEFORE Case C tail
        // must fail closed (`CorruptCommittedFrame`, `LBR-IO-001`) with zero
        // disk mutation!
        // ------------------------------------------------------------------
        let corrupt_repo = tempdir().expect("create corrupt prefix repo");
        let corrupt_init = init_capture_queue(corrupt_repo.path()).expect("init corrupt repo");
        let base_frame = encode_frame(FRAME_FLAG_NONE, "rb08:case-c:base", b"base-committed")
            .expect("encode base");
        let tail_frame = encode_frame(FRAME_FLAG_NONE, "rb08:case-c:tail", b"uncommitted-tail")
            .expect("encode tail");
        append_encoded_frame(corrupt_repo.path(), CaptureQueueLane::Normal, &base_frame)
            .expect("commit base");
        let mut tail_crash = SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
        let _ = append_validated_frame_in_storage_dir(
            corrupt_repo.path(),
            CaptureQueueLane::Normal,
            &tail_frame,
            &mut tail_crash,
        )
        .expect_err("crash after syncing tail");

        // Flip 1 bit inside committed prefix `[0, base_frame.len())` and plant a
        // stale `.tmp` file to verify Phase 1 zero-disk-mutation invariant.
        let corrupt_seg = corrupt_init.paths.normal.join(segment_log_file_name(0));
        let mut raw = fs::read(&corrupt_seg).expect("read corrupt seg");
        raw[base_frame.len() - 1] ^= 0x01;
        fs::write(&corrupt_seg, &raw).expect("write bitflipped committed prefix");
        let stale_tmp = corrupt_init.paths.normal.join(QUEUE_TAIL_META_TMP_FILE);
        fs::write(&stale_tmp, b"stale-tmp-must-stay").expect("plant stale tmp");

        let pre_pri = fs::read(tail_meta_path(&corrupt_init.paths.normal)).expect("pre pri");
        let pre_bak = fs::read(tail_meta_bak_path(&corrupt_init.paths.normal)).expect("pre bak");

        let err = recover_lane(corrupt_repo.path(), CaptureQueueLane::Normal)
            .expect_err("bitflipped committed prefix must fail closed before Case C recovery");
        assert!(
            matches!(err, StorageBackendError::CorruptCommittedFrame { .. }),
            "expected CorruptCommittedFrame, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&corrupt_seg).expect("post corrupt seg"),
            raw,
            "corrupted segment must remain byte-for-byte untouched"
        );
        assert_eq!(
            fs::read(tail_meta_path(&corrupt_init.paths.normal)).expect("post pri"),
            pre_pri
        );
        assert_eq!(
            fs::read(tail_meta_bak_path(&corrupt_init.paths.normal)).expect("post bak"),
            pre_bak
        );
        assert!(
            stale_tmp.exists(),
            "Phase 1 fail-closed must NOT delete stale .tmp files"
        );

        // ------------------------------------------------------------------
        // Scenario 5 (Unix security & zero-mutation hardening):
        // (a) Permissive (0755) private/root/lane directories fail closed
        //     without mutating directory permissions or files;
        // (b) Permissive (0644) append.lock fails closed without fchmod or
        //     mutating any file;
        // (c) Missing append.lock on a corrupt lane fails closed with
        //     CorruptCommittedFrame and whole-directory snapshot proving
        //     append.lock was NOT created and zero files changed.
        // ------------------------------------------------------------------
        #[cfg(unix)]
        {
            use std::{collections::BTreeMap, os::unix::fs::PermissionsExt as _};

            fn snapshot_tree(root: &Path) -> BTreeMap<String, (u32, Option<Vec<u8>>)> {
                fn walk(
                    base: &Path,
                    cur: &Path,
                    out: &mut BTreeMap<String, (u32, Option<Vec<u8>>)>,
                ) {
                    for entry in fs::read_dir(cur).expect("read_dir snapshot") {
                        let entry = entry.expect("dir entry snapshot");
                        let p = entry.path();
                        let rel = p
                            .strip_prefix(base)
                            .expect("strip prefix")
                            .to_string_lossy()
                            .to_string();
                        let meta = fs::symlink_metadata(&p).expect("lstat snapshot");
                        let mode = meta.permissions().mode() & 0o7777;
                        if meta.is_dir() {
                            out.insert(rel, (mode, None));
                            walk(base, &p, out);
                        } else {
                            out.insert(
                                rel,
                                (mode, Some(fs::read(&p).expect("read snapshot file"))),
                            );
                        }
                    }
                }
                let mut map = BTreeMap::new();
                walk(root, root, &mut map);
                map
            }

            // (a) Permissive `private/`, `capture-queue/`, and `normal/` dirs fail closed.
            let perm_repo = tempdir().expect("create permissive dir repo");
            let perm_init = init_capture_queue(perm_repo.path()).expect("init perm repo");
            let private_dir = perm_repo.path().join(PRIVATE_SUBDIR);
            for dir_to_widen in [
                private_dir.as_path(),
                perm_init.paths.root.as_path(),
                perm_init.paths.normal.as_path(),
            ] {
                fs::set_permissions(dir_to_widen, fs::Permissions::from_mode(0o755))
                    .expect("widen dir to 0755");
                let snap_before = snapshot_tree(perm_repo.path());
                let err = append_frame(
                    perm_repo.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    "rb08:perm-dir",
                    b"blocked",
                )
                .expect_err("permissive 0755 queue directory must fail closed");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    snapshot_tree(perm_repo.path()),
                    snap_before,
                    "permissive directory fail-closed must leave disk unchanged"
                );
                fs::set_permissions(dir_to_widen, fs::Permissions::from_mode(0o700))
                    .expect("restore dir to 0700");
            }

            // (b) Permissive `0644` `append.lock` fails closed with zero disk mutation.
            let lock_path = perm_init.paths.root.join(QUEUE_APPEND_LOCK_FILE);
            fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644))
                .expect("widen append.lock to 0644");
            let snap_before_lock = snapshot_tree(perm_repo.path());
            let lock_err = recover_lane(perm_repo.path(), CaptureQueueLane::Normal)
                .expect_err("permissive 0644 append.lock must fail closed without fchmod");
            assert_eq!(lock_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                snapshot_tree(perm_repo.path()),
                snap_before_lock,
                "permissive append.lock fail-closed must not fchmod or mutate disk"
            );

            // (c) Missing `append.lock` on a corrupt lane fails closed with zero
            //     mutation (must NOT create `append.lock`).
            fs::remove_file(corrupt_init.paths.root.join(QUEUE_APPEND_LOCK_FILE))
                .expect("remove append.lock from corrupt repo");
            let snap_corrupt_before = snapshot_tree(corrupt_repo.path());
            let missing_lock_err = recover_lane(corrupt_repo.path(), CaptureQueueLane::Normal)
                .expect_err("corrupt lane with missing append.lock must fail closed");
            assert!(matches!(
                missing_lock_err,
                StorageBackendError::CorruptCommittedFrame { .. }
            ));
            assert_eq!(missing_lock_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                snapshot_tree(corrupt_repo.path()),
                snap_corrupt_before,
                "fail-closed classification with missing append.lock must create zero files"
            );
            assert!(
                !corrupt_init
                    .paths
                    .root
                    .join(QUEUE_APPEND_LOCK_FILE)
                    .exists(),
                "append.lock must not be created when Phase 1 fails closed"
            );

            // (d) Controlled missing-lock race: `append.lock` is initially
            //     absent on a valid lane, and a concurrent racer plants a
            //     permissive `0644` `append.lock` strictly between read-only
            //     pre-classification and exclusive creation -> must fail closed
            //     WITHOUT `fchmod`ing the racer's permissive lock!
            let race_repo = tempdir().expect("create missing-lock race repo");
            let race_init = init_capture_queue(race_repo.path()).expect("init race repo");
            let race_lock_path = race_init.paths.root.join(QUEUE_APPEND_LOCK_FILE);
            fs::remove_file(&race_lock_path).expect("remove append.lock to open race window");
            let lock_path_clone = race_lock_path.clone();
            let mut race_obs = SegmentObserver::with_before_create_missing_lock(move || {
                fs::write(&lock_path_clone, b"").expect("racer creates append.lock");
                fs::set_permissions(&lock_path_clone, fs::Permissions::from_mode(0o644))
                    .expect("racer sets permissive 0644 mode");
            });
            let race_err = recover_lane_in_storage_dir(
                race_repo.path(),
                CaptureQueueLane::Normal,
                &mut race_obs,
            )
            .expect_err("racer planting permissive 0644 append.lock must fail closed");
            assert_eq!(race_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::symlink_metadata(&race_lock_path)
                    .expect("stat raced append.lock")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o644,
                "lost exclusive lock creation race must never fchmod the racer's permissive lock"
            );
        }
    }

    #[test]
    fn publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops() {
        let repo = tempdir().expect("create RB-08t ordering repo");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let quarantine_dir = &init.paths.quarantine;
        let normal_dir = &init.paths.normal;
        let priority_dir = &init.paths.priority;

        // ------------------------------------------------------------------
        // AC1, AC2, AC3: Direct `publish_quarantine_durable` ordering & exact bytes
        // ------------------------------------------------------------------
        let damaged_direct = b"\xDE\xAD\xBE\xEFhalf-frame-uncommitted-tail-bytes-0123456789";
        let direct_id = quarantine_artifact_id(CaptureQueueLane::Normal, 0, 128, damaged_direct);
        assert!(direct_id.starts_with("normal-seg00000000-off128-"));

        let mut direct_obs = SegmentObserver::new();
        let direct_report = publish_quarantine_durable_inner(
            quarantine_dir,
            &direct_id,
            damaged_direct,
            &mut direct_obs,
        )
        .expect("publish_quarantine_durable must succeed");
        assert_eq!(direct_report.id, direct_id);
        assert_eq!(direct_report.quarantined_bytes, damaged_direct.len() as u64);
        assert_eq!(direct_report.crc32c, crc32c_checksum(damaged_direct));
        assert_eq!(direct_report.diagnostic_code, QUARANTINE_DIAGNOSTIC_CODE);

        let sync_complete_seq = direct_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineTmpSyncDataComplete {
                    id: direct_id.clone(),
                }
            })
            .expect("tmp sync_data complete seq");
        let rename_start_seq = direct_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameStart {
                    id: direct_id.clone(),
                }
            })
            .expect("rename start seq");
        let rename_complete_seq = direct_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameComplete {
                    id: direct_id.clone(),
                }
            })
            .expect("rename complete seq");
        let fsync_dir_start_seq = direct_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirStart {
                    id: direct_id.clone(),
                }
            })
            .expect("fsync_dir start seq");
        let fsync_dir_complete_seq = direct_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: direct_id.clone(),
                }
            })
            .expect("fsync_dir complete seq");

        // AC1: tmp_file.sync_data() completion seq < rename(.tmp -> .bin) start seq
        assert!(
            sync_complete_seq < rename_start_seq,
            "AC1 violated: sync_complete={sync_complete_seq} must be < rename_start={rename_start_seq}"
        );
        // AC2: rename(.tmp -> .bin) completion seq < fsync_dir(quarantine_dir) start seq
        assert!(
            rename_complete_seq < fsync_dir_start_seq,
            "AC2 violated: rename_complete={rename_complete_seq} must be < fsync_dir_start={fsync_dir_start_seq}"
        );
        assert!(fsync_dir_start_seq < fsync_dir_complete_seq);

        // AC3: exact byte equality on quarantine/<id>.bin + no leftover .tmp + 0600 mode
        let actual_quar_bytes =
            fs::read(&direct_report.bin_path).expect("read published quarantine .bin");
        assert_eq!(actual_quar_bytes.as_slice(), damaged_direct);
        assert!(
            !quarantine_dir.join(format!("{direct_id}.tmp")).exists(),
            ".tmp must be atomically renamed away"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::symlink_metadata(&direct_report.bin_path)
                .expect("stat quarantine .bin")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, QUEUE_FILE_MODE, "quarantine .bin must be 0600");
        }

        // ------------------------------------------------------------------
        // AC4: `quarantine_and_truncate_segment` ordered barrier
        //      fsync_dir(quarantine_dir) completion seq < truncate(events-<S>) start seq
        // ------------------------------------------------------------------
        let valid_frame = encode_frame(FRAME_FLAG_NONE, "rb08t:valid:1", b"committed-valid-frame")
            .expect("encode valid frame");
        let out = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &valid_frame)
            .expect("append valid frame");
        let valid_end = out.meta.committed_len;
        let tail_garbage = b"LCE1\x99\x00\x00\x00partial-half-frame-tail";
        let seg0_path = normal_dir.join(segment_log_file_name(0));
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open seg0");
            f.write_all(tail_garbage).expect("append tail garbage");
            f.sync_data().expect("sync tail garbage");
        }

        let mut trunc_obs = SegmentObserver::new();
        let trunc_report = quarantine_and_truncate_segment_inner(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
            &mut trunc_obs,
        )
        .expect("quarantine_and_truncate_segment must succeed");
        assert_eq!(
            fs::read(&trunc_report.bin_path).expect("read truncated tail .bin"),
            tail_garbage
        );
        assert_eq!(
            fs::read(&seg0_path).expect("read truncated seg0"),
            valid_frame
        );

        let trunc_sync_done = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineTmpSyncDataComplete {
                    id: trunc_report.id.clone(),
                }
            })
            .expect("trunc sync_data complete");
        let trunc_rename_start = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameStart {
                    id: trunc_report.id.clone(),
                }
            })
            .expect("trunc rename start");
        let trunc_rename_done = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameComplete {
                    id: trunc_report.id.clone(),
                }
            })
            .expect("trunc rename complete");
        let trunc_fsync_start = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirStart {
                    id: trunc_report.id.clone(),
                }
            })
            .expect("trunc fsync_dir start");
        let trunc_fsync_done = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: trunc_report.id.clone(),
                }
            })
            .expect("trunc fsync_dir complete");
        let truncate_start_seq = trunc_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                    to_len: valid_end,
                }
            })
            .expect("truncate start seq");

        assert!(trunc_sync_done < trunc_rename_start);
        assert!(trunc_rename_done < trunc_fsync_start);
        assert!(
            trunc_fsync_done < truncate_start_seq,
            "AC4 violated: fsync_dir_complete={trunc_fsync_done} must be < truncate_start={truncate_start_seq}"
        );

        // ------------------------------------------------------------------
        // AC5: `quarantine_and_unlink_orphan` ordered barrier
        //      fsync_dir(quarantine_dir) completion seq < unlink(orphan_seg) start seq
        // ------------------------------------------------------------------
        let orphan_bytes = b"LCE1\x10\x00\x00\x00orphan-half-frame-bytes";
        let orphan_path = priority_dir.join(segment_log_file_name(1));
        fs::write(&orphan_path, orphan_bytes).expect("create damaged orphan seg1");

        let mut unlink_obs = SegmentObserver::new();
        let unlink_report = quarantine_and_unlink_orphan_inner(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            1,
            &mut unlink_obs,
        )
        .expect("quarantine_and_unlink_orphan must succeed");
        assert_eq!(
            fs::read(&unlink_report.bin_path).expect("read orphan .bin"),
            orphan_bytes
        );
        assert!(
            !orphan_path.exists(),
            "orphan segment must be unlinked after durable quarantine"
        );

        let unlink_sync_done = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineTmpSyncDataComplete {
                    id: unlink_report.id.clone(),
                }
            })
            .expect("unlink sync_data complete");
        let unlink_rename_start = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameStart {
                    id: unlink_report.id.clone(),
                }
            })
            .expect("unlink rename start");
        let unlink_rename_done = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameComplete {
                    id: unlink_report.id.clone(),
                }
            })
            .expect("unlink rename complete");
        let unlink_fsync_start = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirStart {
                    id: unlink_report.id.clone(),
                }
            })
            .expect("unlink fsync_dir start");
        let unlink_fsync_done = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: unlink_report.id.clone(),
                }
            })
            .expect("unlink fsync_dir complete");
        let orphan_unlink_start_seq = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkStart {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 1,
                }
            })
            .expect("orphan unlink start seq");
        let orphan_unlink_done_seq = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkComplete {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 1,
                }
            })
            .expect("orphan unlink complete seq");
        let lane_fsync_after_unlink = unlink_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                    lane: QUEUE_LANE_PRIORITY_DIR,
                    seg_id: 1,
                }
            })
            .expect("lane fsync after orphan unlink");

        assert!(unlink_sync_done < unlink_rename_start);
        assert!(unlink_rename_done < unlink_fsync_start);
        assert!(
            unlink_fsync_done < orphan_unlink_start_seq,
            "AC5 violated: fsync_dir_complete={unlink_fsync_done} must be < unlink_start={orphan_unlink_start_seq}"
        );
        assert!(orphan_unlink_done_seq < lane_fsync_after_unlink);

        // Multi-chunk streaming verification (> 2× 64 KiB chunk size) proves
        // `quarantine_and_truncate_segment` and `quarantine_and_unlink_orphan`
        // stream arbitrary damaged byte spans without unbounded heap allocation.
        let large_tail: Vec<u8> = (0..(140 * 1024usize))
            .map(|i| ((i * 31 + 17) & 0xff) as u8)
            .collect();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open seg0 for large tail");
            f.write_all(&large_tail)
                .expect("append 140 KiB damaged tail");
            f.sync_data().expect("sync 140 KiB damaged tail");
        }
        let large_trunc_report = quarantine_and_truncate_segment(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
        )
        .expect("streaming 140 KiB quarantine_and_truncate_segment must succeed");
        assert_eq!(
            large_trunc_report.quarantined_bytes,
            large_tail.len() as u64
        );
        assert_eq!(large_trunc_report.crc32c, crc32c_checksum(&large_tail));
        assert_eq!(
            fs::read(&large_trunc_report.bin_path).expect("read 140 KiB quarantined tail"),
            large_tail
        );
        assert_eq!(
            fs::read(&seg0_path).expect("read truncated seg0"),
            valid_frame
        );

        let large_orphan: Vec<u8> = (0..(140 * 1024usize))
            .map(|i| ((i * 19 + 7) & 0xff) as u8)
            .collect();
        let large_orphan_path = priority_dir.join(segment_log_file_name(2));
        fs::write(&large_orphan_path, &large_orphan).expect("write 140 KiB damaged orphan");
        let large_orphan_report = quarantine_and_unlink_orphan(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            2,
        )
        .expect("streaming 140 KiB quarantine_and_unlink_orphan must succeed");
        assert_eq!(
            large_orphan_report.quarantined_bytes,
            large_orphan.len() as u64
        );
        assert_eq!(large_orphan_report.crc32c, crc32c_checksum(&large_orphan));
        assert_eq!(
            fs::read(&large_orphan_report.bin_path).expect("read 140 KiB quarantined orphan"),
            large_orphan
        );
        assert!(!large_orphan_path.exists());

        // Concurrent contention regression test: 8 concurrent workers racing on
        // the SAME quarantine artifact id, distinct artifact ids, AND concurrent
        // `recover_lane(Normal)` / `recover_lane(Priority)` cleanup must serialize
        // cleanly under `append.lock` with zero lost `.bin` artifacts or errors.
        let shared_payload = b"concurrent-shared-quarantine-payload-exact-bytes".to_vec();
        let shared_id = quarantine_artifact_id(CaptureQueueLane::Normal, 9, 4096, &shared_payload);
        let qdir_arc = std::sync::Arc::new(quarantine_dir.to_path_buf());
        let repo_arc = std::sync::Arc::new(repo.path().to_path_buf());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::with_capacity(8);
        for worker in 0..8usize {
            let qdir_c = std::sync::Arc::clone(&qdir_arc);
            let repo_c = std::sync::Arc::clone(&repo_arc);
            let bar_c = std::sync::Arc::clone(&barrier);
            let shared_id_c = shared_id.clone();
            let shared_payload_c = shared_payload.clone();
            handles.push(std::thread::spawn(move || {
                bar_c.wait();
                for iter in 0..4usize {
                    let r1 = publish_quarantine_durable(&qdir_c, &shared_id_c, &shared_payload_c)
                        .expect("concurrent shared-id publish must succeed");
                    assert_eq!(r1.quarantined_bytes, shared_payload_c.len() as u64);

                    let unique_payload = format!("worker-{worker}-iter-{iter}-bytes").into_bytes();
                    let lane = if worker % 2 == 0 {
                        CaptureQueueLane::Normal
                    } else {
                        CaptureQueueLane::Priority
                    };
                    let uid = quarantine_artifact_id(
                        lane,
                        100 + worker as u32,
                        iter as u64,
                        &unique_payload,
                    );
                    let r2 = publish_quarantine_durable(&qdir_c, &uid, &unique_payload)
                        .expect("concurrent unique-id publish must succeed");
                    assert_eq!(
                        fs::read(&r2.bin_path).expect("read concurrent unique .bin"),
                        unique_payload
                    );

                    let _ = recover_lane(&repo_c, lane).expect(
                        "concurrent recover_lane alongside quarantine publish must succeed",
                    );
                }
            }));
        }
        for handle in handles {
            handle
                .join()
                .expect("concurrent quarantine worker panicked");
        }
        assert_eq!(
            fs::read(quarantine_dir.join(format!("{shared_id}.bin")))
                .expect("shared concurrent .bin must persist intact"),
            shared_payload
        );
        assert!(
            !quarantine_dir.join(format!("{shared_id}.tmp")).exists(),
            "no leftover .tmp after concurrent publish + recover"
        );

        // Security checks: invalid artifact id and symlinked target/tmp fail closed.
        assert!(
            publish_quarantine_durable(quarantine_dir, "../escape", b"bad").is_err(),
            "path-traversal id must fail closed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let decoy = tempdir().expect("create decoy dir");
            let evil_target = decoy.path().join("evil");
            symlink(&evil_target, quarantine_dir.join("normal-sym.tmp"))
                .expect("plant symlink at .tmp");
            let sym_err = publish_quarantine_durable(quarantine_dir, "normal-sym", b"bytes")
                .expect_err("symlinked quarantine .tmp must fail closed");
            assert!(
                crate::storage::queue::is_capture_queue_symlink_rejected_error(&sym_err),
                "expected symlink rejected error, got {sym_err:?}"
            );
            assert!(!evil_target.exists());
            fs::remove_file(quarantine_dir.join("normal-sym.tmp")).expect("clean symlink");
        }
    }

    #[test]
    fn publish_quarantine_durable_crash_before_rename_and_after_dir_fsync() {
        let repo = tempdir().expect("create RB-08t crash boundary repo");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let quarantine_dir = &init.paths.quarantine;
        let normal_dir = &init.paths.normal;
        let priority_dir = &init.paths.priority;

        // ------------------------------------------------------------------
        // Part 1 — AC6 (Same-lane restart cleanup + cross-lane `.tmp` isolation):
        // Commit F1 on Normal and P1 on Priority; write a valid Case C synced
        // frame F2 on Normal without updating tail.meta; inject crashes strictly
        // BEFORE `rename` for BOTH lanes on the exact same repo. Restarting
        // Normal via `recover_lane(repo, Normal)` must:
        //   (1) recover Case C frame F2 on Normal,
        //   (2) clean ONLY `quarantine/normal-*.tmp` (cleaned == 1),
        //   (3) preserve `quarantine/priority-*.tmp` untouched!
        // Restarting Priority via `recover_lane(repo, Priority)` then cleans
        // `quarantine/priority-*.tmp`.
        // ------------------------------------------------------------------
        let frame_n1 =
            encode_frame(FRAME_FLAG_NONE, "rb08t:ac6:n1", b"normal-base-frame").expect("encode N1");
        let frame_n2 = encode_frame(FRAME_FLAG_NONE, "rb08t:ac6:n2", b"normal-case-c-synced")
            .expect("encode N2");
        let frame_p1 = encode_frame(FRAME_FLAG_NONE, "rb08t:ac6:p1", b"priority-base-frame")
            .expect("encode P1");

        let _out_n1 = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &frame_n1)
            .expect("commit N1");
        let out_p1 = append_encoded_frame(repo.path(), CaptureQueueLane::Priority, &frame_p1)
            .expect("commit P1");

        let seg0_path = normal_dir.join(segment_log_file_name(0));
        let mut expected_n1_n2 = frame_n1.clone();
        expected_n1_n2.extend_from_slice(&frame_n2);
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open normal seg0");
            f.write_all(&frame_n2).expect("write Case C synced N2");
            f.sync_data().expect("sync Case C N2");
        }

        let ac6_normal_bytes = b"LCE1\x11normal-pre-rename-crash-sample";
        let ac6_priority_bytes = b"LCE1\x22priority-pre-rename-crash-sample";
        let ac6_normal_id = quarantine_artifact_id(
            CaptureQueueLane::Normal,
            0,
            frame_n1.len() as u64,
            ac6_normal_bytes,
        );
        let ac6_priority_id = quarantine_artifact_id(
            CaptureQueueLane::Priority,
            0,
            out_p1.meta.committed_len,
            ac6_priority_bytes,
        );
        let ac6_normal_tmp = quarantine_dir.join(format!("{ac6_normal_id}.tmp"));
        let ac6_normal_bin = quarantine_dir.join(format!("{ac6_normal_id}.bin"));
        let ac6_priority_tmp = quarantine_dir.join(format!("{ac6_priority_id}.tmp"));
        let ac6_priority_bin = quarantine_dir.join(format!("{ac6_priority_id}.bin"));

        let mut crash_ac6_n =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let err_ac6_n = publish_quarantine_durable_inner(
            quarantine_dir,
            &ac6_normal_id,
            ac6_normal_bytes,
            &mut crash_ac6_n,
        )
        .expect_err("AC6 normal before-rename crash must fail");
        assert_eq!(err_ac6_n.stable_code_str(), "LBR-IO-001");

        let mut crash_ac6_p =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let err_ac6_p = publish_quarantine_durable_inner(
            quarantine_dir,
            &ac6_priority_id,
            ac6_priority_bytes,
            &mut crash_ac6_p,
        )
        .expect_err("AC6 priority before-rename crash must fail");
        assert_eq!(err_ac6_p.stable_code_str(), "LBR-IO-001");

        assert!(ac6_normal_tmp.exists());
        assert!(!ac6_normal_bin.exists());
        assert!(ac6_priority_tmp.exists());
        assert!(!ac6_priority_bin.exists());
        assert_eq!(
            fs::read(&seg0_path).expect("read normal seg0"),
            expected_n1_n2
        );

        // Restart the exact same Normal lane on the exact same repo.
        let normal_rec = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("AC6: same-lane Normal restart must succeed");
        assert_eq!(
            normal_rec.recovered_frames, 1,
            "Case C synced frame N2 recovered"
        );
        assert_eq!(normal_rec.meta.committed_len, expected_n1_n2.len() as u64);
        assert_eq!(
            normal_rec.cleaned_quarantine_tmp_files, 1,
            "AC6: same-lane Normal restart must clean normal-*.tmp"
        );
        assert!(
            !ac6_normal_tmp.exists(),
            "AC6: normal-*.tmp must be removed"
        );
        assert!(
            ac6_priority_tmp.exists(),
            "AC6: Normal restart must NEVER remove priority-*.tmp"
        );

        // Restart the exact same Priority lane on the exact same repo.
        let priority_rec = recover_lane(repo.path(), CaptureQueueLane::Priority)
            .expect("AC6: same-lane Priority restart must succeed");
        assert_eq!(priority_rec.meta.committed_len, out_p1.meta.committed_len);
        assert_eq!(priority_rec.cleaned_quarantine_tmp_files, 1);
        assert!(
            !ac6_priority_tmp.exists(),
            "AC6: priority-*.tmp must be removed"
        );

        // ------------------------------------------------------------------
        // Part 2 — AC7 (Before-rename crash on truncate AND orphan unlink paths
        // leaves original segments 100% byte-for-byte untouched on the same repo):
        // ------------------------------------------------------------------
        let valid_end = normal_rec.meta.committed_len;
        let damaged_tail = b"LCE1\x20\x00\x00\x00truncated-uncommitted-tail";
        let mut expected_pre_crash_seg0 = expected_n1_n2.clone();
        expected_pre_crash_seg0.extend_from_slice(damaged_tail);
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open seg0");
            f.write_all(damaged_tail).expect("append damaged tail");
            f.sync_data().expect("sync damaged tail");
        }

        let expected_trunc_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 0, valid_end, damaged_tail);
        let trunc_tmp_path = quarantine_dir.join(format!("{expected_trunc_id}.tmp"));
        let trunc_bin_path = quarantine_dir.join(format!("{expected_trunc_id}.bin"));

        let mut crash_before_rename_trunc =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let err = quarantine_and_truncate_segment_inner(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
            &mut crash_before_rename_trunc,
        )
        .expect_err("crash before rename must fail");
        assert_eq!(err.stable_code_str(), "LBR-IO-001");

        // AC7: original segment is 100% byte-for-byte untouched (zero premature truncate)
        assert_eq!(
            fs::read(&seg0_path).expect("read seg0 after crash before rename"),
            expected_pre_crash_seg0,
            "AC7 violated: segment bytes changed despite crash before rename"
        );
        assert!(
            trunc_tmp_path.exists(),
            "crash before rename must leave .tmp"
        );
        assert!(
            !trunc_bin_path.exists(),
            "crash before rename must not publish .bin"
        );

        let damaged_orphan = b"LCE1\x30\x00\x00\x00half-frame-orphan-bytes";
        let orphan_path = priority_dir.join(segment_log_file_name(1));
        fs::write(&orphan_path, damaged_orphan).expect("write damaged orphan");
        let expected_orphan_id =
            quarantine_artifact_id(CaptureQueueLane::Priority, 1, 0, damaged_orphan);
        let orphan_tmp_path = quarantine_dir.join(format!("{expected_orphan_id}.tmp"));
        let orphan_bin_path = quarantine_dir.join(format!("{expected_orphan_id}.bin"));

        let mut crash_before_rename_orphan =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let orphan_err = quarantine_and_unlink_orphan_inner(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            1,
            &mut crash_before_rename_orphan,
        )
        .expect_err("crash before orphan quarantine rename must fail");
        assert_eq!(orphan_err.stable_code_str(), "LBR-IO-001");

        // AC7: orphan segment is 100% byte-for-byte untouched (zero premature unlink)
        assert_eq!(
            fs::read(&orphan_path).expect("read orphan after crash before rename"),
            damaged_orphan,
            "AC7 violated: orphan segment altered or unlinked despite crash before rename"
        );
        assert!(orphan_tmp_path.exists(), "orphan .tmp must exist on disk");
        assert!(
            !orphan_bin_path.exists(),
            "orphan .bin must not exist before rename"
        );

        // ------------------------------------------------------------------
        // Part 3 — AC8 (Crash strictly AFTER `fsync_dir(quarantine_dir)` and
        // BEFORE destructive `truncate`/`unlink` -> reopen the EXACT SAME lane
        // BEFORE any replay and assert the original `.bin` inode + exact
        // `damaged_bytes` survive untouched, then verify idempotent replay +
        // post-reopen live `append_frame`!):
        // ------------------------------------------------------------------
        #[cfg(unix)]
        fn inode_key(path: &Path) -> (u64, u64) {
            use std::os::unix::fs::MetadataExt as _;
            let md = fs::symlink_metadata(path).expect("stat quarantine .bin");
            (md.dev(), md.ino())
        }

        // 1) Direct publish crash strictly after `fsync_dir(quarantine_dir)`.
        let ac8_direct_bytes = b"LCE1\x44ac8-direct-survives-clean-lane-reopen";
        let ac8_direct_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 0, valid_end, ac8_direct_bytes);
        let ac8_direct_bin = quarantine_dir.join(format!("{ac8_direct_id}.bin"));
        let mut crash_after_fsync_direct = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let err_after_fsync_direct = publish_quarantine_durable_inner(
            quarantine_dir,
            &ac8_direct_id,
            ac8_direct_bytes,
            &mut crash_after_fsync_direct,
        )
        .expect_err("AC8 direct crash after fsync_dir must fail");
        assert_eq!(err_after_fsync_direct.stable_code_str(), "LBR-IO-001");
        #[cfg(unix)]
        let ac8_direct_ino_before_reopen = inode_key(&ac8_direct_bin);

        // 2) Truncate path crash strictly after `fsync_dir(quarantine_dir)` and
        // before `truncate(events-00000000.log)`.
        let mut crash_after_fsync_trunc = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let err_after_fsync_trunc = quarantine_and_truncate_segment_inner(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
            &mut crash_after_fsync_trunc,
        )
        .expect_err("crash after fsync_dir before truncate must fail");
        assert_eq!(err_after_fsync_trunc.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&trunc_bin_path).expect("AC8: read persisted truncate .bin"),
            damaged_tail
        );
        assert!(!trunc_tmp_path.exists());
        assert_eq!(
            fs::read(&seg0_path).expect("seg0 untouched before truncate"),
            expected_pre_crash_seg0
        );

        // 3) Orphan unlink path crash strictly after `fsync_dir(quarantine_dir)`
        // and before `unlink(events-00000001.log)` so ALL THREE original `.bin`
        // artifacts coexist simultaneously BEFORE any lane reopen or replay.
        let mut crash_after_fsync_orphan = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let err_after_fsync_orphan = quarantine_and_unlink_orphan_inner(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            1,
            &mut crash_after_fsync_orphan,
        )
        .expect_err("crash after fsync_dir before unlink must fail");
        assert_eq!(err_after_fsync_orphan.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&orphan_bin_path).expect("AC8: read persisted orphan .bin"),
            damaged_orphan
        );
        assert!(!orphan_tmp_path.exists());
        assert_eq!(
            fs::read(&orphan_path).expect("orphan untouched before unlink"),
            damaged_orphan
        );

        // Reopen BOTH lanes BEFORE any manual replay:
        // - Normal (`RB-08m` Case C Step C1 + `RB-08n` Step C2) automatically
        //   completes idempotent durable quarantine + truncate + `sync_data()` +
        //   Step C1 `LTM2` + Step C2 seal seg 0 into `sealed[]` and rotate to
        //   empty seg 1 without duplicating `.bin` or touching `ac8_direct_bin`.
        // - Priority (`RB-08f` Case D half-frame orphan recovery) automatically completes
        //   idempotent orphan quarantine + unlink + dual-replica sync to `out_p1.meta`.
        let pre_replay_normal_rec = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("AC8: reopening Normal lane completes RB-08m/RB-08n Case C recovery cleanly");
        assert_eq!(pre_replay_normal_rec.meta.active_seg, 1);
        assert_eq!(pre_replay_normal_rec.meta.committed_len, 0);
        assert_eq!(pre_replay_normal_rec.meta.prefix_crc32c, 0);
        assert_eq!(
            pre_replay_normal_rec.meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: valid_end,
                sealed_crc32c: crc32c_checksum(&expected_n1_n2),
            }]
        );
        assert_eq!(pre_replay_normal_rec.quarantined_artifacts.len(), 1);
        assert_eq!(
            pre_replay_normal_rec.quarantined_artifacts[0].id,
            expected_trunc_id
        );
        assert_eq!(
            pre_replay_normal_rec.quarantined_artifacts[0].diagnostic_code,
            QUARANTINE_DIAGNOSTIC_CODE
        );
        assert_eq!(
            fs::read(&seg0_path).expect("seg0 truncated during Normal lane reopen"),
            expected_n1_n2
        );
        assert_eq!(
            fs::read(normal_dir.join(segment_log_file_name(1)))
                .expect("seg1 created empty by Step C2"),
            Vec::<u8>::new()
        );
        let pre_replay_priority_rec = recover_lane(repo.path(), CaptureQueueLane::Priority)
            .expect("AC8: reopening Priority lane completes RB-08f orphan recovery cleanly");
        assert_eq!(pre_replay_priority_rec.meta, out_p1.meta);
        assert_eq!(pre_replay_priority_rec.quarantined_artifacts.len(), 1);
        assert_eq!(
            pre_replay_priority_rec.quarantined_artifacts[0].id,
            expected_orphan_id
        );
        assert!(
            !orphan_path.exists(),
            "orphan seg1 unlinked during Priority lane reopen"
        );

        // Verify `ac8_direct_bin` kept its exact original inode and bytes across
        // BOTH lane reopens, and both `trunc_bin_path` and `orphan_bin_path`
        // persist exact damaged bytes with zero duplicates.
        #[cfg(unix)]
        assert_eq!(
            inode_key(&ac8_direct_bin),
            ac8_direct_ino_before_reopen,
            "AC8: direct .bin inode must survive both pre-replay lane reopens unchanged"
        );
        assert_eq!(
            fs::read(&ac8_direct_bin)
                .expect("AC8: direct .bin bytes after both pre-replay reopens"),
            ac8_direct_bytes
        );
        assert_eq!(
            fs::read(&trunc_bin_path)
                .expect("AC8: truncate .bin bytes after both pre-replay reopens"),
            damaged_tail
        );
        assert_eq!(
            fs::read(&orphan_bin_path)
                .expect("AC8: orphan .bin bytes after both pre-replay reopens"),
            damaged_orphan
        );
        let pre_replay_normal_meta = read_converged_lane_tail_meta(normal_dir)
            .expect("AC8: normal tail.meta remains converged after pre-replay reopens");
        assert_eq!(pre_replay_normal_meta.active_seg, 1);
        assert_eq!(pre_replay_normal_meta.committed_len, 0);
        assert_eq!(pre_replay_normal_meta.sealed.len(), 1);
        assert_eq!(pre_replay_normal_meta.sealed[0].sealed_len, valid_end);
        let pre_replay_priority_meta = read_converged_lane_tail_meta(priority_dir)
            .expect("AC8: priority tail.meta remains converged after pre-replay reopens");
        assert_eq!(
            pre_replay_priority_meta.committed_len,
            out_p1.meta.committed_len
        );

        // Direct idempotent replay of `quarantine_and_truncate_segment` after an
        // `AfterQuarantineDirFsyncBeforeDestructive` crash on a fresh damaged tail.
        let replay_tail_bytes = b"LCE1\x29\x00\x00\x00direct-replay-tail";
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("append direct replay tail");
            f.write_all(replay_tail_bytes)
                .expect("write direct replay tail");
            f.sync_data().expect("sync direct replay tail");
        }
        let mut crash_replay_trunc = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let _ = quarantine_and_truncate_segment_inner(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
            &mut crash_replay_trunc,
        )
        .expect_err("crash after fsync_dir before direct truncate replay must fail");
        let replay_trunc = quarantine_and_truncate_segment(
            quarantine_dir,
            normal_dir,
            CaptureQueueLane::Normal,
            0,
            valid_end,
        )
        .expect("idempotent replay of quarantine_and_truncate_segment must succeed");
        assert_eq!(
            replay_trunc.id,
            quarantine_artifact_id(CaptureQueueLane::Normal, 0, valid_end, replay_tail_bytes)
        );
        assert_eq!(
            fs::read(&replay_trunc.bin_path).expect("replayed truncate .bin"),
            replay_tail_bytes
        );
        assert_eq!(
            fs::read(&seg0_path).expect("truncated seg0"),
            expected_n1_n2
        );

        // Also verify direct idempotent replay of `quarantine_and_unlink_orphan`
        // on a fresh orphan seg 1 after an `AfterQuarantineDirFsyncBeforeDestructive` crash.
        let replay_orphan_bytes = b"LCE1\x31\x00\x00\x00direct-replay-orphan";
        fs::write(&orphan_path, replay_orphan_bytes).expect("write replay orphan seg1");
        let mut crash_replay_orphan = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let _ = quarantine_and_unlink_orphan_inner(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            1,
            &mut crash_replay_orphan,
        )
        .expect_err("crash after fsync_dir before direct orphan replay must fail");
        let replay_orphan = quarantine_and_unlink_orphan(
            quarantine_dir,
            priority_dir,
            CaptureQueueLane::Priority,
            1,
        )
        .expect("idempotent replay of quarantine_and_unlink_orphan must succeed");
        assert_eq!(
            fs::read(&replay_orphan.bin_path).expect("replayed orphan .bin"),
            replay_orphan_bytes
        );
        assert!(!orphan_path.exists(), "orphan unlinked after replay");

        // Reopen BOTH lanes cleanly (succeeds!) and verify the un-replayed
        // original `ac8_direct_bin` (same original inode!), `trunc_bin_path`,
        // and `orphan_bin_path` all persist intact, plus live appends work.
        let reopened_normal = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("AC8: reopen Normal lane after quarantine+truncate must succeed");
        assert_eq!(reopened_normal.meta.active_seg, 1);
        assert_eq!(reopened_normal.meta.committed_len, 0);
        assert_eq!(reopened_normal.meta.sealed.len(), 1);
        assert_eq!(reopened_normal.cleaned_quarantine_tmp_files, 0);
        let reopened_priority = recover_lane(repo.path(), CaptureQueueLane::Priority)
            .expect("AC8: reopen Priority lane after quarantine+unlink must succeed");
        assert_eq!(
            reopened_priority.meta.committed_len,
            out_p1.meta.committed_len
        );
        assert_eq!(reopened_priority.cleaned_quarantine_tmp_files, 0);
        #[cfg(unix)]
        assert_eq!(
            inode_key(&ac8_direct_bin),
            ac8_direct_ino_before_reopen,
            "AC8: un-replayed .bin inode must persist across both clean lane reopens"
        );
        assert_eq!(
            fs::read(&ac8_direct_bin)
                .expect("AC8: un-replayed .bin bytes after clean lane reopens"),
            ac8_direct_bytes
        );
        assert_eq!(
            fs::read(&trunc_bin_path).expect("AC8: truncate .bin persists after lane reopens"),
            damaged_tail
        );
        assert_eq!(
            fs::read(&orphan_bin_path).expect("AC8: orphan .bin persists after lane reopens"),
            damaged_orphan
        );
        let post_n3 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08t:ac8:post-normal",
            b"post-reopen-normal",
        )
        .expect("AC8: Normal lane accepts new frames after reopen");
        assert_eq!(post_n3.seg_id, 1);
        assert_eq!(post_n3.frame_offset, 0);
        assert_eq!(post_n3.meta.committed_len, post_n3.frame_len as u64);
        let post_p2 = append_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_NONE,
            "rb08t:ac8:post-priority",
            b"post-reopen-priority",
        )
        .expect("AC8: Priority lane accepts new frames after reopen");
        assert!(post_p2.meta.committed_len > out_p1.meta.committed_len);
    }

    #[test]
    fn case_d_old_seg_prefix_guard_and_valid_successor_recovery() {
        #[cfg(unix)]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .expect("create 0600 queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }
        #[cfg(not(unix))]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }

        // ------------------------------------------------------------------
        // AC1 & AC2: Case D old-segment prior guards (short-file AND prefix
        // bitflip in `[0, committed_len)`) MUST fail closed with
        // `CorruptCommittedFrame` (`LBR-IO-001`) BEFORE touching any successor
        // segment and with ZERO disk mutation across the entire queue tree!
        // ------------------------------------------------------------------
        let guard_repo = tempdir().expect("create RB-08d prior-guard repo");
        let guard_init = init_capture_queue(guard_repo.path()).expect("init prior-guard queue");
        let guard_normal = &guard_init.paths.normal;
        let guard_quarantine = &guard_init.paths.quarantine;

        let old_f1 =
            encode_frame(FRAME_FLAG_NONE, "rb08d:old:1", b"old-seg-committed-frame-1").unwrap();
        let old_f2 =
            encode_frame(FRAME_FLAG_NONE, "rb08d:old:2", b"old-seg-committed-frame-2").unwrap();
        append_encoded_frame(guard_repo.path(), CaptureQueueLane::Normal, &old_f1)
            .expect("commit old frame 1");
        let old_out = append_encoded_frame(guard_repo.path(), CaptureQueueLane::Normal, &old_f2)
            .expect("commit old frame 2");
        let old_seg0_path = guard_normal.join(segment_log_file_name(0));
        let old_seg0_orig = fs::read(&old_seg0_path).expect("read original seg0");
        assert_eq!(old_seg0_orig.len() as u64, old_out.meta.committed_len);

        let succ_f1 = encode_frame(
            FRAME_FLAG_NONE,
            "rb08d:succ:1",
            b"valid-successor-frame-alpha",
        )
        .unwrap();
        let succ_f2 = encode_frame(
            FRAME_FLAG_NONE,
            "rb08d:succ:2",
            b"valid-successor-frame-beta",
        )
        .unwrap();
        let mut succ_bytes = Vec::new();
        succ_bytes.extend_from_slice(&succ_f1);
        succ_bytes.extend_from_slice(&succ_f2);

        let guard_succ_path = guard_normal.join(segment_log_file_name(1));
        write_file_0600(&guard_succ_path, &succ_bytes);
        // Also plant stale .tmp files to prove Phase 1 fail-closed leaves them untouched.
        write_file_0600(&guard_normal.join(QUEUE_TAIL_META_TMP_FILE), b"stale-tmp");
        write_file_0600(&guard_quarantine.join("normal-stale-q.tmp"), b"stale-q-tmp");

        // AC1(a): Truncate old segment `events-00000000.log` to `committed_len - 1`.
        {
            let f = fs::OpenOptions::new()
                .write(true)
                .open(&old_seg0_path)
                .expect("open old seg0 for truncation");
            f.set_len(old_out.meta.committed_len - 1)
                .expect("truncate old seg0 by 1 byte");
            f.sync_data().expect("sync truncated old seg0");
        }
        let ac1_before = snapshot_tree(&guard_init.paths.root);
        let ac1_rec_err = recover_lane(guard_repo.path(), CaptureQueueLane::Normal)
            .expect_err("AC1: short old segment must fail closed");
        assert!(
            matches!(
                ac1_rec_err,
                StorageBackendError::CorruptCommittedFrame { .. }
            ),
            "AC1 expected CorruptCommittedFrame, got {ac1_rec_err:?}"
        );
        assert_eq!(ac1_rec_err.stable_code_str(), "LBR-IO-001");
        let ac1_app_err = append_frame(
            guard_repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08d:ac1:blocked",
            b"must-not-write",
        )
        .expect_err("AC1: append on short old segment must fail closed");
        assert!(matches!(
            ac1_app_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(ac1_app_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            snapshot_tree(&guard_init.paths.root),
            ac1_before,
            "AC1 violated: short old segment failure mutated queue disk tree"
        );

        // AC1(b): Prove prior guard stops BEFORE touching successor `events-00000001.log`
        // by replacing `events-00000001.log` with a symlink while seg0 is short ->
        // MUST still return `CorruptCommittedFrame` (NOT symlink error)!
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&guard_succ_path).expect("remove regular succ");
            symlink(
                guard_repo.path().join("nonexistent-target"),
                &guard_succ_path,
            )
            .expect("plant symlink at succ");
            let sym_prior_err = recover_lane(guard_repo.path(), CaptureQueueLane::Normal)
                .expect_err("AC1: short old seg must fail before opening symlinked successor");
            assert!(
                matches!(
                    sym_prior_err,
                    StorageBackendError::CorruptCommittedFrame { .. }
                ),
                "AC1 must fail on old seg BEFORE touching successor, got {sym_prior_err:?}"
            );
            assert_eq!(sym_prior_err.stable_code_str(), "LBR-IO-001");
            fs::remove_file(&guard_succ_path).expect("remove symlinked succ");
            write_file_0600(&guard_succ_path, &succ_bytes);
        }

        // AC2(a): Restore old segment length `committed_len` and flip a single bit
        // inside `[0, committed_len)` -> `prefix_crc32c` mismatch MUST fail closed
        // with `CorruptCommittedFrame` (`LBR-IO-001`) and zero disk mutation.
        let mut flipped_old = old_seg0_orig.clone();
        let flip_idx = flipped_old.len() / 2;
        flipped_old[flip_idx] ^= 0x01;
        write_file_0600(&old_seg0_path, &flipped_old);

        let ac2_before = snapshot_tree(&guard_init.paths.root);
        let ac2_rec_err = recover_lane(guard_repo.path(), CaptureQueueLane::Normal)
            .expect_err("AC2: old segment prefix bitflip must fail closed");
        assert!(
            matches!(
                ac2_rec_err,
                StorageBackendError::CorruptCommittedFrame { .. }
            ),
            "AC2 expected CorruptCommittedFrame, got {ac2_rec_err:?}"
        );
        assert_eq!(ac2_rec_err.stable_code_str(), "LBR-IO-001");
        let ac2_app_err = append_frame(
            guard_repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08d:ac2:blocked",
            b"must-not-write",
        )
        .expect_err("AC2: append on prefix-bitflipped old segment must fail closed");
        assert!(matches!(
            ac2_app_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(ac2_app_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            snapshot_tree(&guard_init.paths.root),
            ac2_before,
            "AC2 violated: old segment prefix bitflip failure mutated queue disk tree"
        );

        // AC2(b): Prove prefix CRC prior guard also fires BEFORE opening `S+1`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&guard_succ_path).expect("remove regular succ");
            symlink(
                guard_repo.path().join("nonexistent-target"),
                &guard_succ_path,
            )
            .expect("plant symlink at succ");
            let sym_prior_err = recover_lane(guard_repo.path(), CaptureQueueLane::Normal)
                .expect_err("AC2: prefix bitflip must fail before opening symlinked successor");
            assert!(
                matches!(
                    sym_prior_err,
                    StorageBackendError::CorruptCommittedFrame { .. }
                ),
                "AC2 must fail on old seg prefix CRC BEFORE touching successor, got {sym_prior_err:?}"
            );
            assert_eq!(sym_prior_err.stable_code_str(), "LBR-IO-001");
        }

        // ------------------------------------------------------------------
        // AC3–AC7: Clean old segment (`actual_len(S) == committed_len`) +
        // clean valid successor `events-<S+1>` -> 100% successor frame retention,
        // old segment sealed into `LTM2.sealed[]`, dual replicas advanced to
        // `(ACTIVE_SEG = S + 1, COMMITTED_LEN = succ_len, PREFIX_CRC32C = succ_crc)`!
        // ------------------------------------------------------------------
        let repo = tempdir().expect("create RB-08d clean Case D repo");
        let init = init_capture_queue(repo.path()).expect("init clean Case D queue");
        let normal_dir = &init.paths.normal;
        let quarantine_dir = &init.paths.quarantine;

        let s0_f1 = encode_frame(FRAME_FLAG_NONE, "rb08d:s0:1", b"seg0-frame-one").unwrap();
        let s0_f2 = encode_frame(FRAME_FLAG_NONE, "rb08d:s0:2", b"seg0-frame-two").unwrap();
        append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f1).expect("s0 f1");
        let s0_out =
            append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f2).expect("s0 f2");
        let s0_len = s0_out.meta.committed_len;
        let s0_crc = s0_out.meta.prefix_crc32c;
        let s0_path = normal_dir.join(segment_log_file_name(0));
        let s0_bytes_before = fs::read(&s0_path).expect("read seg0");
        assert_eq!(s0_bytes_before.len() as u64, s0_len);
        assert_eq!(crc32c_checksum(&s0_bytes_before), s0_crc);

        // Simulate crash after rotation created `events-00000001.log`, synced 3
        // complete valid frames + fsync_dir, BEFORE committing dual `LTM2`.
        // Also corrupt `tail.meta.bak` (Subcase B1) + leave stale `.tmp` files
        // to prove dual convergence and Phase 2 lane-scoped cleanup work together.
        let s1_f1 = encode_frame(FRAME_FLAG_NONE, "rb08d:s1:1", b"succ-frame-1-payload").unwrap();
        let s1_f2 = encode_frame(
            FRAME_FLAG_NONE,
            "rb08d:s1:2",
            &vec![0x5Au8; 70 * 1024], // > 64 KiB chunk boundary
        )
        .unwrap();
        let s1_f3 = encode_frame(FRAME_FLAG_NONE, "rb08d:s1:3", b"succ-frame-3-payload").unwrap();
        let mut s1_bytes = Vec::new();
        s1_bytes.extend_from_slice(&s1_f1);
        s1_bytes.extend_from_slice(&s1_f2);
        s1_bytes.extend_from_slice(&s1_f3);
        let s1_len = s1_bytes.len() as u64;
        let s1_crc = crc32c_checksum(&s1_bytes);

        let s1_path = normal_dir.join(segment_log_file_name(1));
        write_file_0600(&s1_path, &s1_bytes);
        write_file_0600(&normal_dir.join(QUEUE_TAIL_META_BAK_FILE), b"corrupt-bak");
        write_file_0600(&normal_dir.join(QUEUE_TAIL_META_TMP_FILE), b"stale-meta");
        write_file_0600(&quarantine_dir.join("normal-case-d.tmp"), b"stale-normal-q");
        write_file_0600(
            &quarantine_dir.join("priority-other.tmp"),
            b"must-preserve-other-lane",
        );

        let report = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("AC3-AC7: clean Case D recovery must succeed");
        assert_eq!(report.lane, CaptureQueueLane::Normal);
        assert_eq!(report.recovered_frames, 3);
        assert_eq!(report.recovered_bytes, s1_len);
        assert_eq!(report.cleaned_tmp_files, 1);
        assert_eq!(report.cleaned_quarantine_tmp_files, 1);
        assert!(
            quarantine_dir.join("priority-other.tmp").exists(),
            "Normal Case D recovery must preserve Priority lane's quarantine .tmp"
        );

        // AC3: 100% of successor valid frame bytes retained untouched on disk
        // (and old segment 0 untouched too).
        assert_eq!(
            fs::read(&s1_path).expect("read retained seg1"),
            s1_bytes,
            "AC3 violated: successor segment bytes altered during Case D recovery"
        );
        assert_eq!(
            fs::read(&s0_path).expect("read sealed seg0"),
            s0_bytes_before,
            "old sealed segment bytes must remain untouched"
        );

        // Decode BOTH raw `tail.meta` and `tail.meta.bak` independently.
        let primary_raw =
            fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).expect("read primary tail.meta");
        let backup_raw =
            fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE)).expect("read backup tail.meta.bak");
        assert_eq!(
            primary_raw, backup_raw,
            "dual LTM2 replicas must match byte-for-byte after Case D commit"
        );
        let primary_meta = LaneTailMeta::decode(&primary_raw).expect("decode primary LTM2");
        let backup_meta = LaneTailMeta::decode(&backup_raw).expect("decode backup LTM2");
        assert_eq!(primary_meta, backup_meta);
        assert_eq!(report.meta, primary_meta);

        // AC4: Clean old segment `(0, s0_len, s0_crc)` sealed into `LTM2.sealed[]`.
        assert_eq!(primary_meta.min_live_seg, 0);
        assert_eq!(
            primary_meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: s0_len,
                sealed_crc32c: s0_crc,
            }],
            "AC4 violated: old segment 0 not accurately sealed into LTM2.sealed[]"
        );
        // AC5: `ACTIVE_SEG` advanced to `S + 1 == 1`.
        assert_eq!(primary_meta.active_seg, 1, "AC5 violated: active_seg != 1");
        // AC6: `COMMITTED_LEN` advanced to exact `s1_len`.
        assert_eq!(
            primary_meta.committed_len, s1_len,
            "AC6 violated: committed_len != succ_len"
        );
        // AC7: `PREFIX_CRC32C` matches exact `crc32c(events-00000001.log)`.
        assert_eq!(
            primary_meta.prefix_crc32c, s1_crc,
            "AC7 violated: prefix_crc32c != crc32c(succ_bytes)"
        );

        // Verify direct `append_frame` on a SECOND Case D transition (`S=1 -> S=2`)
        // appends seamlessly after the recovered frames and accumulates `sealed[] = [0, 1]`.
        let s2_f1 = encode_frame(FRAME_FLAG_NONE, "rb08d:s2:1", b"seg2-recovered-frame").unwrap();
        let s2_path = normal_dir.join(segment_log_file_name(2));
        write_file_0600(&s2_path, &s2_f1);

        let s2_f2_payload = b"seg2-appended-after-case-d";
        let app_out = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08d:s2:2",
            s2_f2_payload,
        )
        .expect("direct append_frame through Case D must recover seg2, seal seg1, and append");
        assert_eq!(app_out.seg_id, 2);
        assert_eq!(app_out.frame_offset, s2_f1.len() as u64);
        assert_eq!(app_out.recovered_frames, 1);
        assert_eq!(app_out.meta.active_seg, 2);
        assert_eq!(
            app_out.meta.sealed,
            vec![
                SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: s0_len,
                    sealed_crc32c: s0_crc,
                },
                SealedSegmentMeta {
                    seg_id: 1,
                    sealed_len: s1_len,
                    sealed_crc32c: s1_crc,
                },
            ]
        );
        let s2_disk = fs::read(&s2_path).expect("read seg2 after append");
        assert_eq!(s2_disk.len() as u64, app_out.meta.committed_len);
        assert_eq!(crc32c_checksum(&s2_disk), app_out.meta.prefix_crc32c);
        let mut cursor = std::io::Cursor::new(&s2_disk);
        let mut dec = FrameStreamDecoder::with_known_len(&mut cursor, 0, s2_disk.len() as u64);
        let FrameStreamItem::Frame { frame: dec1, .. } = dec.next_frame().unwrap() else {
            panic!("expected recovered frame 1");
        };
        let FrameStreamItem::Frame { frame: dec2, .. } = dec.next_frame().unwrap() else {
            panic!("expected newly appended frame 2");
        };
        assert_eq!(dec1.dedup_key, "rb08d:s2:1");
        assert_eq!(dec2.dedup_key, "rb08d:s2:2");
        assert_eq!(dec2.payload, s2_f2_payload);
        assert!(matches!(
            dec.next_frame().unwrap(),
            FrameStreamItem::CleanEof { .. }
        ));

        // ------------------------------------------------------------------
        // Phase 1 fail-closed edge guards:
        // (1) Non-contiguous successor gap (`active_seg=2`, `events-00000004.log`)
        // (2) Multiple successors (`events-00000003.log` + `events-00000004.log`)
        // (3) Illegal full-table state (`sealed.len() == 256` + valid successor)
        // (4) Empty 0-byte old segment + valid successor
        // All four MUST fail closed with `CorruptCommittedFrame` (`LBR-IO-001`)
        // and ZERO disk mutation!
        // ------------------------------------------------------------------
        let gap_path = normal_dir.join(segment_log_file_name(4));
        write_file_0600(&gap_path, &s2_f1);
        let gap_before = snapshot_tree(&init.paths.root);
        let gap_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect_err("non-contiguous Case D gap must fail closed");
        assert!(matches!(
            gap_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(gap_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(snapshot_tree(&init.paths.root), gap_before);

        let s3_path = normal_dir.join(segment_log_file_name(3));
        write_file_0600(&s3_path, &s2_f1);
        let multi_before = snapshot_tree(&init.paths.root);
        let multi_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect_err("multiple Case D successors must fail closed");
        assert!(matches!(
            multi_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(multi_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(snapshot_tree(&init.paths.root), multi_before);

        // Full sealed table (`sealed.len() == 256`) + valid successor `257`
        let full_repo = tempdir().expect("create full-table Case D repo");
        let full_init = init_capture_queue(full_repo.path()).expect("init full-table queue");
        let full_normal = &full_init.paths.normal;
        let full_quarantine = &full_init.paths.quarantine;
        let sealed_256: Vec<SealedSegmentMeta> = (0..MAX_SEALED_SEGMENTS as u32)
            .map(|id| SealedSegmentMeta {
                seg_id: id,
                sealed_len: s0_f1.len() as u64,
                sealed_crc32c: crc32c_checksum(&s0_f1),
            })
            .collect();
        let active_256 = MAX_SEALED_SEGMENTS as u32;
        let full_meta = LaneTailMeta {
            min_live_seg: 0,
            active_seg: active_256,
            committed_len: s0_f1.len() as u64,
            prefix_crc32c: crc32c_checksum(&s0_f1),
            sealed: sealed_256,
        };
        let encoded_full_meta = full_meta.encode().expect("encode 256-sealed LTM2");
        write_file_0600(&full_normal.join(QUEUE_TAIL_META_FILE), &encoded_full_meta);
        write_file_0600(
            &full_normal.join(QUEUE_TAIL_META_BAK_FILE),
            &encoded_full_meta,
        );
        write_file_0600(&full_normal.join(segment_log_file_name(active_256)), &s0_f1);
        write_file_0600(
            &full_normal.join(segment_log_file_name(active_256 + 1)),
            &s2_f1,
        );
        write_file_0600(&full_normal.join(QUEUE_TAIL_META_TMP_FILE), b"keep-tmp");
        write_file_0600(&full_quarantine.join("normal-keep.tmp"), b"keep-q-tmp");

        let full_before = snapshot_tree(&full_init.paths.root);
        let full_err = recover_lane(full_repo.path(), CaptureQueueLane::Normal)
            .expect_err("Case D valid successor at MAX_SEALED_SEGMENTS must fail closed");
        assert!(matches!(
            full_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(full_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            snapshot_tree(&full_init.paths.root),
            full_before,
            "Case D full-table fail-closed must leave disk tree 100% untouched"
        );

        // Empty 0-byte old segment `S=0` + valid successor `S=1`
        let zero_repo = tempdir().expect("create 0-byte old seg Case D repo");
        let zero_init = init_capture_queue(zero_repo.path()).expect("init 0-byte repo");
        let zero_normal = &zero_init.paths.normal;
        write_file_0600(&zero_normal.join(segment_log_file_name(0)), b"");
        write_file_0600(&zero_normal.join(segment_log_file_name(1)), &s2_f1);
        let zero_before = snapshot_tree(&zero_init.paths.root);
        let zero_err = recover_lane(zero_repo.path(), CaptureQueueLane::Normal)
            .expect_err("Case D with 0-byte old segment must fail closed");
        assert!(matches!(
            zero_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(zero_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(snapshot_tree(&zero_init.paths.root), zero_before);
    }

    #[test]
    fn case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering() {
        #[cfg(unix)]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .expect("create 0600 queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }
        #[cfg(not(unix))]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }

        let repo = tempdir().expect("create RB-08u ordering repo");
        let init = init_capture_queue(repo.path()).expect("init RB-08u queue");
        let normal_dir = &init.paths.normal;
        let quarantine_dir = &init.paths.quarantine;

        // Commit clean old segment S = 0.
        let s0_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:s0:1", b"old-seg-committed-1").unwrap();
        let s0_out =
            append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f1).expect("commit s0");
        let s0_len = s0_out.meta.committed_len;
        let s0_crc = s0_out.meta.prefix_crc32c;

        // Build successor S + 1 = 1 containing 2 valid complete frames (including
        // a > 64 KiB payload) followed by an uncommitted truncated half-frame tail.
        let s1_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:s1:1", b"succ-valid-frame-1").unwrap();
        let s1_f2 = encode_frame(FRAME_FLAG_NONE, "rb08u:s1:2", &vec![0xA5u8; 72 * 1024]).unwrap();
        let mut s1_valid_prefix = Vec::new();
        s1_valid_prefix.extend_from_slice(&s1_f1);
        s1_valid_prefix.extend_from_slice(&s1_f2);
        let succ_valid_end = s1_valid_prefix.len() as u64;
        let succ_valid_crc = crc32c_checksum(&s1_valid_prefix);

        // Partial uncommitted 3rd frame (header valid, body cut off halfway).
        let s1_f3_full = encode_frame(
            FRAME_FLAG_NONE,
            "rb08u:s1:3-half",
            b"uncommitted-tail-payload-xyz",
        )
        .unwrap();
        let s1_half_tail = s1_f3_full[..s1_f3_full.len() / 2].to_vec();
        assert!(!s1_half_tail.is_empty());

        let mut s1_on_disk = s1_valid_prefix.clone();
        s1_on_disk.extend_from_slice(&s1_half_tail);
        let s1_path = normal_dir.join(segment_log_file_name(1));
        write_file_0600(&s1_path, &s1_on_disk);

        // Execute REAL Case D recovery caller path with ordering instrumentation.
        let mut obs = SegmentObserver::new();
        let report = recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs)
            .expect("AC1-AC3: Case D valid successor with half-frame tail must recover cleanly");

        // AC1: `events-<S+1>` on disk equals exact valid prefix `[0..succ_valid_end)`.
        assert_eq!(
            fs::read(&s1_path).expect("read truncated events-00000001.log"),
            s1_valid_prefix,
            "AC1 violated: events-<S+1> bytes do not equal [0..succ_valid_end)"
        );
        assert_eq!(report.recovered_frames, 2);
        assert_eq!(report.recovered_bytes, succ_valid_end);

        // AC2: `quarantine/<id>.bin` equals exact `[succ_valid_end, actual_len(S+1))`.
        assert_eq!(report.quarantined_artifacts.len(), 1);
        let q_art = &report.quarantined_artifacts[0];
        let expected_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 1, succ_valid_end, &s1_half_tail);
        assert_eq!(q_art.id, expected_id);
        assert_eq!(
            q_art.bin_path,
            quarantine_dir.join(format!("{expected_id}.bin"))
        );
        assert_eq!(q_art.quarantined_bytes, s1_half_tail.len() as u64);
        assert_eq!(q_art.crc32c, crc32c_checksum(&s1_half_tail));
        assert_eq!(q_art.diagnostic_code, QUARANTINE_DIAGNOSTIC_CODE);
        assert_eq!(
            fs::read(&q_art.bin_path).expect("AC2: read quarantine .bin"),
            s1_half_tail,
            "AC2 violated: quarantine .bin bytes do not equal damaged successor tail"
        );
        assert!(
            !quarantine_dir.join(format!("{expected_id}.tmp")).exists(),
            "no leftover quarantine .tmp after successful publish"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::symlink_metadata(&q_art.bin_path)
                .expect("stat quarantine .bin")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, QUEUE_FILE_MODE, "quarantine .bin must be 0600");
        }

        // AC3: Caller-path sequence instrumentation proves:
        //   tmp.sync_data() complete < rename start
        //   rename complete < fsync_dir(quarantine_dir) start
        //   fsync_dir(quarantine_dir) complete < truncate(events-<S+1>) start
        //   truncate(events-<S+1>) complete < tail.meta tmp sync
        let sync_done = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineTmpSyncDataComplete {
                    id: expected_id.clone(),
                }
            })
            .expect("quarantine tmp sync complete event");
        let rename_start = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameStart {
                    id: expected_id.clone(),
                }
            })
            .expect("quarantine rename start event");
        let rename_done = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameComplete {
                    id: expected_id.clone(),
                }
            })
            .expect("quarantine rename complete event");
        let fsync_q_start = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirStart {
                    id: expected_id.clone(),
                }
            })
            .expect("quarantine fsync_dir start event");
        let fsync_q_done = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: expected_id.clone(),
                }
            })
            .expect("quarantine fsync_dir complete event");
        let trunc_start = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                    to_len: succ_valid_end,
                }
            })
            .expect("segment truncate start event");
        let trunc_done = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateComplete {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                    to_len: succ_valid_end,
                }
            })
            .expect("segment truncate complete event");
        let primary_meta_sync = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                }
            })
            .expect("primary tail.meta commit event");

        assert!(sync_done < rename_start);
        assert!(rename_done < fsync_q_start);
        assert!(
            fsync_q_done < trunc_start,
            "AC3 violated: fsync_dir(quarantine_dir) complete={fsync_q_done} must be < truncate(S+1) start={trunc_start}"
        );
        assert!(trunc_start < trunc_done);
        assert!(trunc_done < primary_meta_sync);

        // Verify converged dual LTM2 metadata sealed seg 0 and advanced to seg 1.
        let primary_meta = LaneTailMeta::decode(
            &fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).expect("read primary tail.meta"),
        )
        .expect("decode primary");
        let backup_meta = LaneTailMeta::decode(
            &fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE))
                .expect("read backup tail.meta.bak"),
        )
        .expect("decode backup");
        assert_eq!(primary_meta, backup_meta);
        assert_eq!(primary_meta.active_seg, 1);
        assert_eq!(primary_meta.committed_len, succ_valid_end);
        assert_eq!(primary_meta.prefix_crc32c, succ_valid_crc);
        assert_eq!(
            primary_meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: s0_len,
                sealed_crc32c: s0_crc,
            }]
        );

        // Also verify REAL `append_validated_frame_in_storage_dir` caller path on
        // a second transition `S=1 -> S=2` with a valid frame + corrupt-header tail!
        let s2_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:s2:1", b"seg2-valid-frame").unwrap();
        let s2_corrupt_tail = b"BAD!\xff\xff\xff\xffgarbled-uncommitted-tail-bytes".to_vec();
        let mut s2_disk_before = s2_f1.clone();
        s2_disk_before.extend_from_slice(&s2_corrupt_tail);
        let s2_path = normal_dir.join(segment_log_file_name(2));
        write_file_0600(&s2_path, &s2_disk_before);

        let new_f = encode_frame(
            FRAME_FLAG_NONE,
            "rb08u:s2:2",
            b"new-frame-after-case-d-trim",
        )
        .unwrap();
        let mut app_obs = SegmentObserver::new();
        let app_out = append_validated_frame_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &new_f,
            &mut app_obs,
        )
        .expect("append caller path must durably quarantine+truncate S+1 before appending");

        let s2_q_id = quarantine_artifact_id(
            CaptureQueueLane::Normal,
            2,
            s2_f1.len() as u64,
            &s2_corrupt_tail,
        );
        assert_eq!(
            fs::read(quarantine_dir.join(format!("{s2_q_id}.bin"))).expect("read s2 .bin"),
            s2_corrupt_tail
        );
        let app_fsync_q_done = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: s2_q_id.clone(),
                }
            })
            .expect("append caller quarantine fsync_dir complete");
        let app_trunc_start = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 2,
                    to_len: s2_f1.len() as u64,
                }
            })
            .expect("append caller truncate start");
        let app_write_new = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentWriteAll {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 2,
                    offset: s2_f1.len() as u64,
                    len: new_f.len(),
                }
            })
            .expect("append caller write new frame");
        assert!(app_fsync_q_done < app_trunc_start);
        assert!(app_trunc_start < app_write_new);

        let mut expected_s2_after = s2_f1.clone();
        expected_s2_after.extend_from_slice(&new_f);
        assert_eq!(
            fs::read(&s2_path).expect("read final s2"),
            expected_s2_after
        );
        assert_eq!(app_out.meta.active_seg, 2);
        assert_eq!(app_out.meta.committed_len, expected_s2_after.len() as u64);
        assert_eq!(
            app_out.meta.prefix_crc32c,
            crc32c_checksum(&expected_s2_after)
        );
        assert_eq!(app_out.meta.sealed.len(), 2);
    }

    #[test]
    fn case_d_valid_successor_trailing_half_frame_crash_boundaries() {
        #[cfg(unix)]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .expect("create 0600 queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }
        #[cfg(not(unix))]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }

        let repo = tempdir().expect("create RB-08u crash boundary repo");
        let init = init_capture_queue(repo.path()).expect("init RB-08u crash queue");
        let normal_dir = &init.paths.normal;
        let quarantine_dir = &init.paths.quarantine;

        // Commit clean old segment S = 0.
        let s0_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:cb:s0:1", b"old-seg0-payload").unwrap();
        let s0_out =
            append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f1).expect("commit s0");
        let s0_len = s0_out.meta.committed_len;
        let s0_crc = s0_out.meta.prefix_crc32c;

        // Build successor S + 1 = 1 with 2 valid frames + uncommitted half-frame tail.
        let s1_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:cb:s1:1", b"succ-valid-alpha").unwrap();
        let s1_f2 = encode_frame(FRAME_FLAG_NONE, "rb08u:cb:s1:2", b"succ-valid-beta").unwrap();
        let mut s1_valid_prefix = Vec::new();
        s1_valid_prefix.extend_from_slice(&s1_f1);
        s1_valid_prefix.extend_from_slice(&s1_f2);
        let succ_valid_end = s1_valid_prefix.len() as u64;
        let succ_valid_crc = crc32c_checksum(&s1_valid_prefix);

        // Truncate a genuinely encoded frame halfway through its body so the
        // decoder exercises the true `FrameDecodeError::TruncatedTail` half-frame path.
        let s1_f3_full = encode_frame(
            FRAME_FLAG_NONE,
            "rb08u:cb:s1:3-half",
            b"uncommitted-case-d-half-frame-body-payload",
        )
        .unwrap();
        let s1_half_tail = s1_f3_full[..s1_f3_full.len() / 2].to_vec();
        let mut s1_pre_crash = s1_valid_prefix.clone();
        s1_pre_crash.extend_from_slice(&s1_half_tail);
        let s1_path = normal_dir.join(segment_log_file_name(1));
        write_file_0600(&s1_path, &s1_pre_crash);

        // Plant an unrelated priority-lane quarantine .tmp to assert cross-lane
        // isolation across crash restarts.
        let priority_other_tmp = quarantine_dir.join("priority-other-lane.tmp");
        write_file_0600(&priority_other_tmp, b"priority-tmp-must-stay");

        let expected_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 1, succ_valid_end, &s1_half_tail);
        let expected_tmp = quarantine_dir.join(format!("{expected_id}.tmp"));
        let expected_bin = quarantine_dir.join(format!("{expected_id}.bin"));

        // ------------------------------------------------------------------
        // AC4: Crash strictly BEFORE `rename(.tmp -> .bin)` inside real Case D
        // recovery -> `events-<S+1>` MUST remain 100% byte-for-byte identical
        // (zero premature truncate!), `.tmp` remains, `.bin` absent, `LTM2` at S=0.
        // ------------------------------------------------------------------
        let mut crash_before_rename =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let err_ac4 = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut crash_before_rename,
        )
        .expect_err("AC4: crash before quarantine rename must fail");
        assert_eq!(err_ac4.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&s1_path).expect("AC4: read events-00000001.log after pre-rename crash"),
            s1_pre_crash,
            "AC4 violated: events-<S+1> was prematurely modified or truncated before rename"
        );
        assert!(
            expected_tmp.exists(),
            "AC4: .tmp must remain after pre-rename crash"
        );
        assert!(
            !expected_bin.exists(),
            "AC4: .bin must not exist before rename"
        );
        assert_eq!(
            read_converged_lane_tail_meta(normal_dir).expect("AC4: tail.meta still at S=0"),
            s0_out.meta
        );
        assert!(priority_other_tmp.exists());

        // ------------------------------------------------------------------
        // AC5: Crash strictly AFTER `fsync_dir(quarantine_dir)` and BEFORE
        // `truncate(events-<S+1>)` inside real Case D recovery -> `.bin`
        // persists durably with exact `[succ_valid_end, actual_len(S+1))`,
        // `events-<S+1>` still untrimmed, `LTM2` still at S=0.
        // ------------------------------------------------------------------
        let mut crash_after_fsync = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let err_ac5 = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut crash_after_fsync,
        )
        .expect_err("AC5: crash after quarantine fsync_dir before truncate must fail");
        assert_eq!(err_ac5.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&expected_bin).expect("AC5: persisted quarantine .bin after dir fsync crash"),
            s1_half_tail,
            "AC5 violated: persisted .bin bytes do not equal original damaged half-frame tail"
        );
        assert!(
            !expected_tmp.exists(),
            "AC5: .tmp renamed away before dir fsync"
        );
        assert_eq!(
            fs::read(&s1_path).expect("AC5: events-<S+1> still untrimmed before truncate"),
            s1_pre_crash
        );
        assert_eq!(
            read_converged_lane_tail_meta(normal_dir).expect("AC5: tail.meta still at S=0"),
            s0_out.meta
        );

        // ------------------------------------------------------------------
        // AC5 (reopen) + AC6 + AC7: Reopen the EXACT SAME lane after the crash
        // -> converges cleanly without duplicating `.bin`, trims `events-<S+1>`
        // to exact `succ_valid_end`, seals S=0, and advances dual replicas to
        // `COMMITTED_LEN == succ_valid_end` and `PREFIX_CRC32C == crc32c(valid_prefix)`!
        // ------------------------------------------------------------------
        let report = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("AC5-AC7: reopening lane after dir-fsync crash must converge cleanly");
        assert_eq!(report.recovered_frames, 2);
        assert_eq!(report.recovered_bytes, succ_valid_end);
        assert_eq!(report.quarantined_artifacts.len(), 1);
        assert_eq!(report.quarantined_artifacts[0].id, expected_id);
        assert_eq!(
            fs::read(&expected_bin).expect("AC5: quarantine .bin after reopen convergence"),
            s1_half_tail
        );
        assert_eq!(
            fs::read(&s1_path).expect("reopened truncated events-<S+1>"),
            s1_valid_prefix
        );
        assert!(
            priority_other_tmp.exists(),
            "Normal Case D restart must preserve priority-other-lane.tmp"
        );

        // Confirm zero duplicate `.bin` artifacts were created in `quarantine/`.
        let normal_bins: Vec<_> = fs::read_dir(quarantine_dir)
            .expect("read quarantine dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with("normal-") && s.ends_with(".bin")
            })
            .collect();
        assert_eq!(
            normal_bins.len(),
            1,
            "deterministic id must prevent duplicate quarantine .bin files on crash replay"
        );

        // AC6 & AC7: Decode BOTH raw `tail.meta` and `tail.meta.bak`.
        let primary_raw =
            fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).expect("read converged tail.meta");
        let backup_raw = fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE))
            .expect("read converged tail.meta.bak");
        assert_eq!(primary_raw, backup_raw);
        let primary_meta = LaneTailMeta::decode(&primary_raw).expect("decode converged primary");
        let backup_meta = LaneTailMeta::decode(&backup_raw).expect("decode converged backup");
        assert_eq!(primary_meta, backup_meta);
        assert_eq!(primary_meta.active_seg, 1);
        assert_eq!(
            primary_meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: s0_len,
                sealed_crc32c: s0_crc,
            }]
        );
        assert_eq!(
            primary_meta.committed_len, succ_valid_end,
            "AC6 violated: converged LTM2.COMMITTED_LEN != succ_valid_end"
        );
        assert_eq!(
            primary_meta.prefix_crc32c, succ_valid_crc,
            "AC7 violated: converged LTM2.PREFIX_CRC32C != crc32c(events-<S+1>[0..succ_valid_end])"
        );

        // Also verify direct restart directly from AC4 (`BeforeQuarantineRename`)
        // cleans the leftover `normal-*.tmp` (`cleaned_quarantine_tmp_files == 1`)
        // and converges cleanly on `S=1 -> S=2`!
        let s2_f1 = encode_frame(FRAME_FLAG_NONE, "rb08u:cb:s2:1", b"seg2-valid-frame").unwrap();
        let s2_f2_full = encode_frame(
            FRAME_FLAG_NONE,
            "rb08u:cb:s2:2-half",
            b"seg2-uncommitted-half-frame-payload",
        )
        .unwrap();
        let s2_half_tail = s2_f2_full[..s2_f2_full.len() / 2].to_vec();
        let mut s2_pre_crash = s2_f1.clone();
        s2_pre_crash.extend_from_slice(&s2_half_tail);
        let s2_path = normal_dir.join(segment_log_file_name(2));
        write_file_0600(&s2_path, &s2_pre_crash);

        let mut crash_s2_before_rename =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let _ = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut crash_s2_before_rename,
        )
        .expect_err("AC4 on S=2 before rename must fail");
        assert_eq!(fs::read(&s2_path).expect("read s2"), s2_pre_crash);

        let s2_recover = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("direct restart after AC4 pre-rename crash must converge cleanly");
        assert_eq!(s2_recover.cleaned_quarantine_tmp_files, 1);
        assert_eq!(s2_recover.meta.active_seg, 2);
        assert_eq!(s2_recover.meta.committed_len, s2_f1.len() as u64);
        assert_eq!(s2_recover.meta.prefix_crc32c, crc32c_checksum(&s2_f1));
        assert_eq!(fs::read(&s2_path).expect("truncated s2"), s2_f1);
        let s2_primary_raw =
            fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).expect("read s2 primary tail.meta");
        let s2_backup_raw = fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE))
            .expect("read s2 backup tail.meta.bak");
        assert_eq!(s2_primary_raw, s2_backup_raw);
        let s2_primary = LaneTailMeta::decode(&s2_primary_raw).expect("decode s2 primary");
        let s2_backup = LaneTailMeta::decode(&s2_backup_raw).expect("decode s2 backup");
        assert_eq!(s2_primary, s2_backup);
        assert_eq!(s2_primary.active_seg, 2);
        assert_eq!(s2_primary.committed_len, s2_f1.len() as u64);
        assert_eq!(s2_primary.prefix_crc32c, crc32c_checksum(&s2_f1));
        assert_eq!(s2_primary.sealed.len(), 2);
        assert!(priority_other_tmp.exists());

        // Live append on reopened lane works seamlessly.
        let post = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08u:cb:post",
            b"post-recovery-frame",
        )
        .expect("live append after Case D half-frame recovery must succeed");
        assert_eq!(post.seg_id, 2);
        assert!(post.meta.committed_len > s2_f1.len() as u64);
    }

    #[test]
    fn case_d_empty_and_half_frame_orphan_cleanup_quarantine() {
        #[cfg(unix)]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .expect("create 0600 queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }
        #[cfg(not(unix))]
        fn write_file_0600(path: &Path, bytes: &[u8]) {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create queue test file");
            f.write_all(bytes).expect("write queue test file");
            f.sync_data().expect("sync queue test file");
        }

        let repo = tempdir().expect("create RB-08f orphan cleanup repo");
        let init = init_capture_queue(repo.path()).expect("init RB-08f queue");
        let normal_dir = &init.paths.normal;
        let quarantine_dir = &init.paths.quarantine;

        // Commit intact active segment S = 0.
        let s0_f1 = encode_frame(FRAME_FLAG_NONE, "rb08f:s0:1", b"intact-seg0-frame-1").unwrap();
        let s0_f2 = encode_frame(FRAME_FLAG_NONE, "rb08f:s0:2", b"intact-seg0-frame-2").unwrap();
        append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f1).expect("commit s0 f1");
        let s0_out = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &s0_f2)
            .expect("commit s0 f2");
        let s0_path = normal_dir.join(segment_log_file_name(0));
        let s0_bytes_orig = fs::read(&s0_path).expect("read intact seg0");
        assert_eq!(s0_bytes_orig.len() as u64, s0_out.meta.committed_len);

        // Plant unrelated priority-lane quarantine .tmp to assert cross-lane isolation.
        let priority_other_tmp = quarantine_dir.join("priority-other-orphan.tmp");
        write_file_0600(&priority_other_tmp, b"keep-priority-tmp");

        // ------------------------------------------------------------------
        // Part 1 — AC1, AC2, AC6: Empty 0-byte orphan successor `events-00000001.log`
        // -> unlink + fsync_dir(lane_dir) + sync both LTM2 replicas to intact S=0
        // (zero quarantine artifact created; S=0 NOT sealed).
        // ------------------------------------------------------------------
        let orphan_path = normal_dir.join(segment_log_file_name(1));
        write_file_0600(&orphan_path, b"");
        // Corrupt backup replica (Subcase B1) to verify dual-replica sync repairs it.
        write_file_0600(&normal_dir.join(QUEUE_TAIL_META_BAK_FILE), b"corrupt-bak");

        let mut empty_obs = SegmentObserver::new();
        let empty_report =
            recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut empty_obs)
                .expect("AC1-AC2: empty Case D orphan recovery must succeed");

        // AC1: empty orphan unlinked with zero quarantine files created.
        assert!(
            !orphan_path.exists(),
            "AC1 violated: empty orphan events-00000001.log must be unlinked"
        );
        assert!(
            empty_report.quarantined_artifacts.is_empty(),
            "empty 0-byte orphan must not create a quarantine .bin artifact"
        );
        assert_eq!(
            fs::read(&s0_path).expect("intact seg0 after empty orphan unlink"),
            s0_bytes_orig
        );
        assert!(priority_other_tmp.exists());

        // AC2: `OrphanUnlinkStart < OrphanUnlinkComplete < FsyncLaneDirAfterOrphanUnlink < MetaTmpWriteAndSync`.
        let empty_unlink_start = empty_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC1/AC2: empty orphan unlink start");
        let empty_unlink_done = empty_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkComplete {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC1/AC2: empty orphan unlink complete");
        let empty_lane_fsync = empty_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC2: empty orphan fsync_dir(lane_dir)");
        let empty_meta_sync = empty_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                }
            })
            .expect("AC6: empty orphan dual-meta sync");
        assert!(empty_unlink_start < empty_unlink_done);
        assert!(
            empty_unlink_done < empty_lane_fsync,
            "AC2 violated: fsync_dir(lane_dir) must immediately follow empty orphan unlink"
        );
        assert!(empty_lane_fsync < empty_meta_sync);

        // Verify both on-disk LTM2 replicas match intact S=0 (sealed remains empty).
        let empty_primary_raw = fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).unwrap();
        let empty_backup_raw = fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE)).unwrap();
        assert_eq!(empty_primary_raw, empty_backup_raw);
        let empty_primary = LaneTailMeta::decode(&empty_primary_raw).unwrap();
        assert_eq!(empty_primary, s0_out.meta);
        assert!(empty_primary.sealed.is_empty());

        // ------------------------------------------------------------------
        // Part 2 — AC8: Half-frame-only orphan `events-00000001.log` (genuine
        // truncated `encode_frame` hitting `FrameDecodeError::TruncatedTail`),
        // crash strictly BEFORE `rename` -> `events-00000001.log` MUST remain
        // 100% byte-for-byte identical (zero premature unlink!).
        // ------------------------------------------------------------------
        let full_orphan_frame = encode_frame(
            FRAME_FLAG_NONE,
            "rb08f:half-orphan:1",
            &vec![0x3Cu8; 70 * 1024], // > 64 KiB streaming quarantine verification
        )
        .unwrap();
        let half_orphan_bytes = full_orphan_frame[..full_orphan_frame.len() / 2].to_vec();
        write_file_0600(&orphan_path, &half_orphan_bytes);
        // Remove backup replica (Subcase B1) so AC6 also proves backup repair!
        fs::remove_file(normal_dir.join(QUEUE_TAIL_META_BAK_FILE)).expect("remove backup meta");

        let expected_orphan_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 1, 0, &half_orphan_bytes);
        let expected_orphan_tmp = quarantine_dir.join(format!("{expected_orphan_id}.tmp"));
        let expected_orphan_bin = quarantine_dir.join(format!("{expected_orphan_id}.bin"));

        let mut crash_before_rename =
            SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
        let err_ac8 = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut crash_before_rename,
        )
        .expect_err("AC8: crash before orphan quarantine rename must fail");
        assert_eq!(err_ac8.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            fs::read(&orphan_path).expect("AC8: orphan must still exist and match exact bytes"),
            half_orphan_bytes,
            "AC8 violated: half-frame orphan altered or prematurely unlinked before rename"
        );
        assert!(expected_orphan_tmp.exists());
        assert!(!expected_orphan_bin.exists());
        assert!(priority_other_tmp.exists());

        // ------------------------------------------------------------------
        // Part 3 — AC3, AC4, AC5, AC6, AC7: Reopen lane -> cleans AC8 `.tmp`,
        // publishes `quarantine/<id>.bin` durably BEFORE `unlink(events-00000001.log)`,
        // runs `fsync_dir(lane_dir)`, and syncs BOTH `tail.meta` and `tail.meta.bak`
        // to intact `active_seg == 0`!
        // ------------------------------------------------------------------
        let mut half_obs = SegmentObserver::new();
        let half_report =
            recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut half_obs)
                .expect("AC3-AC7: half-frame orphan recovery must succeed");
        assert_eq!(half_report.cleaned_quarantine_tmp_files, 1);
        assert!(priority_other_tmp.exists());

        // AC3: exact orphan bytes durably published into `quarantine/<id>.bin` + `LBR-AGENT-050`.
        assert_eq!(half_report.quarantined_artifacts.len(), 1);
        let q_art = &half_report.quarantined_artifacts[0];
        assert_eq!(q_art.id, expected_orphan_id);
        assert_eq!(q_art.bin_path, expected_orphan_bin);
        assert_eq!(q_art.quarantined_bytes, half_orphan_bytes.len() as u64);
        assert_eq!(q_art.crc32c, crc32c_checksum(&half_orphan_bytes));
        assert_eq!(q_art.diagnostic_code, QUARANTINE_DIAGNOSTIC_CODE);
        assert_eq!(
            fs::read(&expected_orphan_bin).expect("AC3: read quarantined half-frame orphan .bin"),
            half_orphan_bytes
        );
        assert!(!expected_orphan_tmp.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::symlink_metadata(&expected_orphan_bin)
                .expect("stat orphan .bin")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, QUEUE_FILE_MODE, "quarantine .bin must be 0600");
        }

        // AC4: half-frame orphan unlinked from disk; intact S=0 untouched.
        assert!(
            !orphan_path.exists(),
            "AC4 violated: damaged half-frame orphan events-00000001.log must be unlinked"
        );
        assert_eq!(
            fs::read(&s0_path).expect("intact seg0 after half-frame orphan unlink"),
            s0_bytes_orig
        );

        // AC5 & AC7: caller-path strict ordering instrumentation:
        //   tmp.sync_data complete < rename start
        //   rename complete < fsync_dir(quarantine_dir) start
        //   fsync_dir(quarantine_dir) complete < unlink(events-00000001.log) start
        //   unlink complete < fsync_dir(lane_dir)
        //   fsync_dir(lane_dir) < dual LTM2 commit (primary & backup)
        let q_sync_done = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineTmpSyncDataComplete {
                    id: expected_orphan_id.clone(),
                }
            })
            .expect("AC3/AC7: quarantine tmp sync_data complete");
        let q_rename_start = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameStart {
                    id: expected_orphan_id.clone(),
                }
            })
            .expect("AC3/AC7: quarantine rename start");
        let q_rename_done = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineRenameComplete {
                    id: expected_orphan_id.clone(),
                }
            })
            .expect("AC3/AC7: quarantine rename complete");
        let q_fsync_start = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirStart {
                    id: expected_orphan_id.clone(),
                }
            })
            .expect("AC3/AC7: quarantine fsync_dir start");
        let q_fsync_done = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: expected_orphan_id.clone(),
                }
            })
            .expect("AC7: quarantine fsync_dir complete");
        let half_unlink_start = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC4/AC7: half-frame orphan unlink start");
        let half_unlink_done = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkComplete {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC4/AC5: half-frame orphan unlink complete");
        let half_lane_fsync = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC5: fsync_dir(lane_dir) after half-frame orphan unlink");
        let half_primary_meta = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                }
            })
            .expect("AC6: primary tail.meta sync");
        let half_backup_meta = half_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("AC6: backup tail.meta.bak sync");

        assert!(q_sync_done < q_rename_start);
        assert!(q_rename_done < q_fsync_start);
        assert!(
            q_fsync_done < half_unlink_start,
            "AC7 violated: fsync_dir(quarantine_dir) complete={q_fsync_done} must be < unlink(S+1) start={half_unlink_start}"
        );
        assert!(half_unlink_start < half_unlink_done);
        assert!(
            half_unlink_done < half_lane_fsync,
            "AC5 violated: fsync_dir(lane_dir) must immediately follow half-frame orphan unlink"
        );
        assert!(half_lane_fsync < half_primary_meta);
        assert!(half_primary_meta < half_backup_meta);

        // AC6: Both raw on-disk `tail.meta` and `tail.meta.bak` synced identically
        // to intact `active_seg == 0` (NOT sealed).
        let primary_raw = fs::read(normal_dir.join(QUEUE_TAIL_META_FILE)).expect("read tail.meta");
        let backup_raw =
            fs::read(normal_dir.join(QUEUE_TAIL_META_BAK_FILE)).expect("read tail.meta.bak");
        assert_eq!(primary_raw, backup_raw);
        let primary_meta = LaneTailMeta::decode(&primary_raw).expect("decode primary");
        let backup_meta = LaneTailMeta::decode(&backup_raw).expect("decode backup");
        assert_eq!(primary_meta, backup_meta);
        assert_eq!(primary_meta, s0_out.meta);
        assert_eq!(primary_meta.active_seg, 0);
        assert!(primary_meta.sealed.is_empty());

        // ------------------------------------------------------------------
        // Part 4 — Direct `append_validated_frame_in_storage_dir` caller path
        // cleans half-frame orphan (after an injected dir-fsync crash!) and
        // appends seamlessly onto intact `events-00000000.log`!
        // ------------------------------------------------------------------
        let full_orphan_2 = encode_frame(
            FRAME_FLAG_NONE,
            "rb08f:half-orphan:2",
            b"second-half-frame-orphan-payload",
        )
        .unwrap();
        let half_orphan_2 = full_orphan_2[..full_orphan_2.len() / 2].to_vec();
        write_file_0600(&orphan_path, &half_orphan_2);
        let orphan2_id = quarantine_artifact_id(CaptureQueueLane::Normal, 1, 0, &half_orphan_2);
        let orphan2_bin = quarantine_dir.join(format!("{orphan2_id}.bin"));

        let mut crash_after_fsync = SegmentObserver::with_crash(
            SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
        );
        let _ = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut crash_after_fsync,
        )
        .expect_err("crash after fsync_dir before orphan unlink must fail");
        assert_eq!(fs::read(&orphan2_bin).unwrap(), half_orphan_2);
        assert!(orphan_path.exists());

        let new_f = encode_frame(FRAME_FLAG_NONE, "rb08f:s0:3", b"appended-to-intact-s0").unwrap();
        let mut app_obs = SegmentObserver::new();
        let app_out = append_validated_frame_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &new_f,
            &mut app_obs,
        )
        .expect("append caller path must quarantine+unlink orphan and append to intact S=0");
        assert!(!orphan_path.exists());
        assert_eq!(fs::read(&orphan2_bin).unwrap(), half_orphan_2);
        assert_eq!(app_out.seg_id, 0);
        assert_eq!(app_out.frame_offset, s0_out.meta.committed_len);
        assert_eq!(app_out.meta.active_seg, 0);
        assert!(app_out.meta.sealed.is_empty());

        let app_q_fsync_done = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::QuarantineFsyncDirComplete {
                    id: orphan2_id.clone(),
                }
            })
            .expect("append caller quarantine fsync_dir complete");
        let app_unlink_start = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::OrphanUnlinkStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("append caller orphan unlink start");
        let app_lane_fsync = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("append caller lane fsync after unlink");
        let app_write_s0 = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentWriteAll {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                    offset: s0_out.meta.committed_len,
                    len: new_f.len(),
                }
            })
            .expect("append caller write onto intact seg0");
        assert!(app_q_fsync_done < app_unlink_start);
        assert!(app_unlink_start < app_lane_fsync);
        assert!(app_lane_fsync < app_write_s0);

        // ------------------------------------------------------------------
        // Part 5 — Retry durability when `unlink(orphan)` succeeds and
        // `fsync_dir(lane_dir)` fails (`RB-08f` AC2/AC5 retry durability):
        // retry enters Case A (orphan gone from directory listing), and MUST
        // still execute `fsync_dir(lane_dir)` before returning `Ok`!
        // ------------------------------------------------------------------
        // (a) Empty orphan retry after failed post-unlink `fsync_dir(lane_dir)`
        write_file_0600(&orphan_path, b"");
        let mut fail_fsync_empty_obs =
            SegmentObserver::with_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink);
        let err_empty_fsync = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut fail_fsync_empty_obs,
        )
        .expect_err("injected fsync_dir failure after empty orphan unlink must fail");
        assert_eq!(err_empty_fsync.stable_code_str(), "LBR-IO-001");
        assert!(
            !orphan_path.exists(),
            "orphan was unlinked before fsync_dir failed"
        );
        assert!(
            fail_fsync_empty_obs
                .events
                .contains(&SegmentOpEvent::OrphanUnlinkComplete {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                })
        );
        assert!(!fail_fsync_empty_obs.events.contains(
            &SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                lane: QUEUE_LANE_NORMAL_DIR,
                seg_id: 1,
            }
        ));
        let mut retry_empty_obs = SegmentObserver::new();
        let retry_empty_rec = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut retry_empty_obs,
        )
        .expect("retry recover_lane after failed empty-orphan fsync_dir must succeed");
        assert_eq!(retry_empty_rec.meta, app_out.meta);
        assert!(
            retry_empty_obs
                .events
                .contains(&SegmentOpEvent::FsyncLaneDirOnRecovery {
                    lane: QUEUE_LANE_NORMAL_DIR,
                }),
            "AC2 retry violated: retry after failed post-unlink fsync_dir MUST flush lane_dir before returning Ok"
        );

        // (b) Half-frame orphan retry after failed post-unlink `fsync_dir(lane_dir)`
        write_file_0600(&orphan_path, &half_orphan_2);
        let mut fail_fsync_half_obs =
            SegmentObserver::with_crash(SegmentCrashPoint::FailLaneDirFsyncAfterOrphanUnlink);
        let err_half_fsync = recover_lane_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &mut fail_fsync_half_obs,
        )
        .expect_err("injected fsync_dir failure after half-frame orphan unlink must fail");
        assert_eq!(err_half_fsync.stable_code_str(), "LBR-IO-001");
        assert!(!orphan_path.exists());
        assert_eq!(fs::read(&orphan2_bin).unwrap(), half_orphan_2);
        let mut retry_half_obs = SegmentObserver::new();
        let retry_half_rec =
            recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut retry_half_obs)
                .expect("retry recover_lane after failed half-frame-orphan fsync_dir must succeed");
        assert_eq!(retry_half_rec.meta, app_out.meta);
        assert!(
            retry_half_obs
                .events
                .contains(&SegmentOpEvent::FsyncLaneDirOnRecovery {
                    lane: QUEUE_LANE_NORMAL_DIR,
                }),
            "AC5 retry violated: retry after failed post-unlink fsync_dir MUST flush lane_dir before returning Ok"
        );

        // ------------------------------------------------------------------
        // Part 6 — Prior guard: old segment prefix bitflip + orphan MUST still
        // fail closed with `CorruptCommittedFrame` and ZERO disk mutation!
        // ------------------------------------------------------------------
        write_file_0600(&orphan_path, &half_orphan_2);
        let mut flipped_s0 = fs::read(&s0_path).unwrap();
        flipped_s0[0] ^= 0x01;
        write_file_0600(&s0_path, &flipped_s0);
        let before_corrupt = snapshot_tree(&init.paths.root);
        let prior_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect_err("prefix bitflip on old seg0 must fail closed before touching orphan");
        assert!(matches!(
            prior_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(prior_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(snapshot_tree(&init.paths.root), before_corrupt);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BoundedSnapshotEntry {
        Dir { mode: u32 },
        SmallFile { mode: u32, bytes: Vec<u8> },
        LargeFile { mode: u32, len: u64 },
        Symlink { target: PathBuf },
    }

    fn snapshot_tree_bounded(root: &Path) -> BTreeMap<PathBuf, BoundedSnapshotEntry> {
        const SMALL_FILE_LIMIT: u64 = 1024 * 1024;
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, BoundedSnapshotEntry>) {
            let mut entries: Vec<_> = fs::read_dir(dir)
                .expect("read_dir for bounded snapshot")
                .map(|e| e.expect("dir entry for bounded snapshot"))
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                let rel = p.strip_prefix(base).expect("strip base").to_path_buf();
                let md = fs::symlink_metadata(&p).expect("symlink_metadata for bounded snapshot");
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt as _;
                    md.permissions().mode() & 0o7777
                };
                #[cfg(not(unix))]
                let mode = 0u32;
                if md.file_type().is_symlink() {
                    out.insert(
                        rel,
                        BoundedSnapshotEntry::Symlink {
                            target: fs::read_link(&p).expect("read_link for bounded snapshot"),
                        },
                    );
                } else if md.is_dir() {
                    out.insert(rel, BoundedSnapshotEntry::Dir { mode });
                    walk(base, &p, out);
                } else if md.len() <= SMALL_FILE_LIMIT {
                    out.insert(
                        rel,
                        BoundedSnapshotEntry::SmallFile {
                            mode,
                            bytes: fs::read(&p).expect("read small file for bounded snapshot"),
                        },
                    );
                } else {
                    out.insert(
                        rel,
                        BoundedSnapshotEntry::LargeFile {
                            mode,
                            len: md.len(),
                        },
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn create_sized_file_0600(path: &Path, size: u64) {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .expect("create sized file");
        f.set_len(size).expect("set_len on sized file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("chmod 0600 on sized file");
        }
    }

    fn write_bytes_0600(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("chmod 0600 on file");
        }
    }

    /// `RB-08b` AC1 + AC2:
    /// - AC1: If `unconsumed_priority + unconsumed_normal + quarantine +
    ///   frame_buf.len() > 256 MiB`, immediately reject with `LBR-AGENT-046`
    ///   (`StorageBackendError::QueueFull`) and zero bytes written (zero disk
    ///   mutation), while allowing exact `== 256 MiB` occupancy; also verify
    ///   reclaimed orphans (`seg < min_live_seg`) are excluded and empty Case 0b
    ///   lanes do not create `events-00000000.log` on rejection.
    /// - AC2: Under concurrent `priority/` and `normal/` producers racing for
    ///   exactly 1 remaining frame slot (both at real default `256 MiB` cap and
    ///   under tight multi-round cross-lane caps), at most 1 succeeds, all
    ///   others return `LBR-AGENT-046`, and total occupancy never exceeds the
    ///   cap by even 1 byte.
    #[test]
    fn cross_lane_capacity_boundary_race_and_single_frame_cap() {
        assert_eq!(CAPTURE_QUEUE_MAX_TOTAL_BYTES, 256 * 1024 * 1024);

        // ------------------------------------------------------------------
        // Part 1 — AC1: real default 256 MiB boundary (±1 byte) & zero writes
        // ------------------------------------------------------------------
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");

        let p_seed = append_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb08b-ac1-prio-seed",
            b"priority-seed-payload",
        )
        .expect("append priority seed");
        let n_seed = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08b-ac1-norm-seed",
            b"normal-seed-payload-012345",
        )
        .expect("append normal seed");

        let exact_frame =
            encode_frame(FRAME_FLAG_PRIORITY, "rb08b-ac1-exact-fit", &vec![b'E'; 128])
                .expect("encode exact-fit frame");
        let over_by_one_frame =
            encode_frame(FRAME_FLAG_PRIORITY, "rb08b-ac1-exact-fit", &vec![b'O'; 129])
                .expect("encode over-by-one frame");
        assert_eq!(over_by_one_frame.len(), exact_frame.len() + 1);

        let existing_segs = p_seed.frame_len as u64 + n_seed.frame_len as u64;
        let prefill_len = CAPTURE_QUEUE_MAX_TOTAL_BYTES - existing_segs - exact_frame.len() as u64;
        let prefill_path = init
            .paths
            .quarantine
            .join("normal-seg00000000-off0-00000000.bin");
        create_sized_file_0600(&prefill_path, prefill_len);

        let occ_before =
            measure_capture_queue_occupancy(repo.path()).expect("measure occupancy before");
        assert_eq!(occ_before.priority_bytes, p_seed.frame_len as u64);
        assert_eq!(occ_before.normal_bytes, n_seed.frame_len as u64);
        assert_eq!(occ_before.quarantine_bytes, prefill_len);
        assert_eq!(
            occ_before.total_bytes,
            CAPTURE_QUEUE_MAX_TOTAL_BYTES - exact_frame.len() as u64
        );

        // Over by 1 byte on Priority -> LBR-AGENT-046 + strictly zero disk mutation.
        let snap_before_reject = snapshot_tree_bounded(&init.paths.root);
        let err_prio =
            append_encoded_frame(repo.path(), CaptureQueueLane::Priority, &over_by_one_frame)
                .expect_err("total + frame_len == 256 MiB + 1 on priority must fail");
        assert!(matches!(err_prio, StorageBackendError::QueueFull { .. }));
        assert_eq!(err_prio.stable_code_str(), "LBR-AGENT-046");
        assert_eq!(
            snapshot_tree_bounded(&init.paths.root),
            snap_before_reject,
            "AC1 violated: rejected over-cap priority append must write zero bytes"
        );

        // Over by 1 byte on Normal -> LBR-AGENT-046 + strictly zero disk mutation.
        let err_norm =
            append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &over_by_one_frame)
                .expect_err("total + frame_len == 256 MiB + 1 on normal must fail");
        assert!(matches!(err_norm, StorageBackendError::QueueFull { .. }));
        assert_eq!(err_norm.stable_code_str(), "LBR-AGENT-046");
        assert_eq!(
            snapshot_tree_bounded(&init.paths.root),
            snap_before_reject,
            "AC1 violated: rejected over-cap normal append must write zero bytes"
        );

        // Exact `== 256 MiB` MUST succeed!
        let exact_out = append_encoded_frame(repo.path(), CaptureQueueLane::Priority, &exact_frame)
            .expect("total + frame_len == exactly 256 MiB must succeed");
        assert_eq!(exact_out.frame_len, exact_frame.len());
        let occ_full =
            measure_capture_queue_occupancy(repo.path()).expect("measure occupancy at exact cap");
        assert_eq!(occ_full.total_bytes, CAPTURE_QUEUE_MAX_TOTAL_BYTES);

        // At exact 256 MiB, even a minimal frame on either lane is rejected with zero writes.
        let snap_at_full = snapshot_tree_bounded(&init.paths.root);
        for lane in [CaptureQueueLane::Priority, CaptureQueueLane::Normal] {
            let err_full = append_frame(repo.path(), lane, FRAME_FLAG_NONE, "k", b"x")
                .expect_err("append at full 256 MiB cap must fail");
            assert!(matches!(err_full, StorageBackendError::QueueFull { .. }));
            assert_eq!(err_full.stable_code_str(), "LBR-AGENT-046");
        }
        assert_eq!(
            snapshot_tree_bounded(&init.paths.root),
            snap_at_full,
            "AC1 violated: rejected appends at exact 256 MiB cap must mutate zero bytes"
        );

        // Verify Case 0b fresh empty lane rejection creates zero segment files.
        let empty_repo = tempdir().expect("empty tempdir");
        let empty_init = init_capture_queue(empty_repo.path()).expect("init empty queue");
        create_sized_file_0600(
            &empty_init.paths.quarantine.join("cap-fill.bin"),
            CAPTURE_QUEUE_MAX_TOTAL_BYTES - 8,
        );
        let snap_empty_before = snapshot_tree_bounded(&empty_init.paths.root);
        let empty_err = append_frame(
            empty_repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "empty-lane-over-cap",
            b"payload",
        )
        .expect_err("empty lane over-cap append must fail with LBR-AGENT-046");
        assert!(matches!(empty_err, StorageBackendError::QueueFull { .. }));
        assert_eq!(empty_err.stable_code_str(), "LBR-AGENT-046");
        assert!(
            !empty_init
                .paths
                .priority
                .join(segment_log_file_name(0))
                .exists()
        );
        assert_eq!(
            snapshot_tree_bounded(&empty_init.paths.root),
            snap_empty_before
        );

        // Verify reclaimed orphan (`seg < min_live_seg`) in sibling lane is NOT
        // counted as unconsumed occupancy while live segments >= min_live_seg ARE.
        let orphan_repo = tempdir().expect("orphan tempdir");
        let orphan_init = init_capture_queue(orphan_repo.path()).expect("init orphan queue");
        let live_frame = encode_frame(FRAME_FLAG_NONE, "live-seg1", b"live-payload").unwrap();
        let seg1_path = orphan_init.paths.normal.join(segment_log_file_name(1));
        write_bytes_0600(&seg1_path, &live_frame);
        let live_meta = LaneTailMeta {
            min_live_seg: 1,
            active_seg: 1,
            committed_len: live_frame.len() as u64,
            prefix_crc32c: crc32c_checksum(&live_frame),
            sealed: Vec::new(),
        };
        let encoded_live_meta = live_meta.encode().unwrap();
        write_bytes_0600(
            &tail_meta_path(&orphan_init.paths.normal),
            &encoded_live_meta,
        );
        write_bytes_0600(
            &tail_meta_bak_path(&orphan_init.paths.normal),
            &encoded_live_meta,
        );
        // Reclaimed orphan seg0 (`0 < min_live_seg = 1`) with 4096 bytes.
        create_sized_file_0600(
            &orphan_init.paths.normal.join(segment_log_file_name(0)),
            4096,
        );
        let orphan_occ =
            measure_capture_queue_occupancy(orphan_repo.path()).expect("measure orphan occupancy");
        assert_eq!(
            orphan_occ.normal_bytes,
            live_frame.len() as u64,
            "reclaimed orphan seg0 (< min_live_seg) must not count toward unconsumed occupancy"
        );
        assert_eq!(orphan_occ.total_bytes, live_frame.len() as u64);

        // Verify option clamping cannot weaken the 256 MiB hard cap.
        let clamped_opts = QueueAppendOptions::default().with_max_total_bytes(u64::MAX);
        assert_eq!(
            clamped_opts.effective_max_total_bytes(),
            CAPTURE_QUEUE_MAX_TOTAL_BYTES
        );

        // ------------------------------------------------------------------
        // Part 2a — AC2: 8-thread concurrent priority/normal race at real
        // default 256 MiB cap with exact 1-frame headroom
        // ------------------------------------------------------------------
        let race_repo = tempdir().expect("race tempdir");
        let race_init = init_capture_queue(race_repo.path()).expect("init race queue");
        let seed_p = append_frame(
            race_repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "race-seed-p",
            b"seed-priority",
        )
        .expect("race seed priority");
        let seed_n = append_frame(
            race_repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "race-seed-n",
            b"seed-normal",
        )
        .expect("race seed normal");

        const RACE_THREADS: usize = 8;
        let race_frames: Vec<(CaptureQueueLane, Vec<u8>)> = (0..RACE_THREADS)
            .map(|idx| {
                let lane = if idx % 2 == 0 {
                    CaptureQueueLane::Priority
                } else {
                    CaptureQueueLane::Normal
                };
                let flags = if lane == CaptureQueueLane::Priority {
                    FRAME_FLAG_PRIORITY
                } else {
                    FRAME_FLAG_NONE
                };
                let key = format!("race-256m-{idx:02}");
                let payload = vec![b'R'; 512];
                (
                    lane,
                    encode_frame(flags, &key, &payload).expect("encode race frame"),
                )
            })
            .collect();
        let unit_frame_len = race_frames[0].1.len() as u64;
        assert!(
            race_frames
                .iter()
                .all(|(_, f)| f.len() as u64 == unit_frame_len)
        );

        let race_prefill = CAPTURE_QUEUE_MAX_TOTAL_BYTES
            - seed_p.frame_len as u64
            - seed_n.frame_len as u64
            - unit_frame_len;
        create_sized_file_0600(
            &race_init.paths.quarantine.join("race-prefill.bin"),
            race_prefill,
        );

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(RACE_THREADS));
        let mut handles = Vec::with_capacity(RACE_THREADS);
        for (lane, frame_bytes) in race_frames {
            let repo_path = race_repo.path().to_path_buf();
            let b = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
                append_encoded_frame(&repo_path, lane, &frame_bytes)
            }));
        }

        let mut ok_count = 0usize;
        let mut queue_full_count = 0usize;
        for h in handles {
            match h.join().expect("join race producer thread") {
                Ok(out) => {
                    assert_eq!(out.frame_len as u64, unit_frame_len);
                    ok_count += 1;
                }
                Err(err) => {
                    assert!(
                        matches!(err, StorageBackendError::QueueFull { .. }),
                        "unexpected non-QueueFull race error: {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-AGENT-046");
                    queue_full_count += 1;
                }
            }
        }
        assert_eq!(
            ok_count, 1,
            "AC2 violated: exactly 1 racer should win 1 remaining slot"
        );
        assert_eq!(queue_full_count, RACE_THREADS - 1);

        let race_occ =
            measure_capture_queue_occupancy(race_repo.path()).expect("measure post-race occupancy");
        assert_eq!(
            race_occ.total_bytes, CAPTURE_QUEUE_MAX_TOTAL_BYTES,
            "AC2 violated: combined occupancy must never exceed 256 MiB by even 1 byte"
        );
        // Decode both lanes and verify full frame + LTM2 integrity.
        let mut total_decoded_frames = 0usize;
        for lane_dir in [&race_init.paths.priority, &race_init.paths.normal] {
            let meta = read_converged_lane_tail_meta(lane_dir).expect("read lane meta after race");
            let raw_meta = fs::read(tail_meta_path(lane_dir)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(lane_dir)).unwrap();
            assert_eq!(raw_meta, raw_bak);
            let seg_bytes = fs::read(lane_dir.join(segment_log_file_name(0))).unwrap();
            assert_eq!(seg_bytes.len() as u64, meta.committed_len);
            assert_eq!(crc32c_checksum(&seg_bytes), meta.prefix_crc32c);
            let mut dec = FrameStreamDecoder::new(&seg_bytes[..], meta.committed_len);
            loop {
                match dec.next_frame().expect("decode post-race frame") {
                    FrameStreamItem::Frame { .. } => {
                        total_decoded_frames += 1;
                    }
                    FrameStreamItem::CleanEof { .. } => break,
                }
            }
        }
        assert_eq!(
            total_decoded_frames, 3,
            "2 seeds + 1 winning racer = 3 frames"
        );

        // ------------------------------------------------------------------
        // Part 2b — AC2: 5 repeated rounds of 12-thread cross-lane races with
        // pure segment bytes and headroom `2*L - 1` (strictly < 2 frames!)
        // ------------------------------------------------------------------
        let tight_repo = tempdir().expect("tight race tempdir");
        let _tight_init = init_capture_queue(tight_repo.path()).expect("init tight race queue");
        const TIGHT_THREADS: usize = 12;
        for round in 0..5usize {
            let occ_now =
                measure_capture_queue_occupancy(tight_repo.path()).expect("tight occ before round");
            let round_frames: Vec<(CaptureQueueLane, Vec<u8>)> = (0..TIGHT_THREADS)
                .map(|idx| {
                    let lane = if (idx + round) % 2 == 0 {
                        CaptureQueueLane::Priority
                    } else {
                        CaptureQueueLane::Normal
                    };
                    let flags = if lane == CaptureQueueLane::Priority {
                        FRAME_FLAG_PRIORITY
                    } else {
                        FRAME_FLAG_NONE
                    };
                    let key = format!("tight-r{round:02}-t{idx:02}");
                    let payload = vec![b'T'; 256];
                    (
                        lane,
                        encode_frame(flags, &key, &payload).expect("encode tight frame"),
                    )
                })
                .collect();
            let frame_len = round_frames[0].1.len() as u64;
            assert!(
                round_frames
                    .iter()
                    .all(|(_, f)| f.len() as u64 == frame_len)
            );
            // Allow 1 frame + (frame_len - 1) leftover bytes: strictly < 2 frames!
            let round_cap = occ_now.total_bytes + (frame_len * 2) - 1;
            let opts = QueueAppendOptions::default().with_max_total_bytes(round_cap);

            let b = std::sync::Arc::new(std::sync::Barrier::new(TIGHT_THREADS));
            let mut hs = Vec::with_capacity(TIGHT_THREADS);
            for (lane, frame_bytes) in round_frames {
                let repo_path = tight_repo.path().to_path_buf();
                let b_cl = std::sync::Arc::clone(&b);
                hs.push(std::thread::spawn(move || {
                    b_cl.wait();
                    append_encoded_frame_with_options(&repo_path, lane, &frame_bytes, opts)
                }));
            }
            let mut wins = 0usize;
            let mut rejects = 0usize;
            for h in hs {
                match h.join().expect("join tight racer") {
                    Ok(_) => wins += 1,
                    Err(err) => {
                        assert!(matches!(err, StorageBackendError::QueueFull { .. }));
                        assert_eq!(err.stable_code_str(), "LBR-AGENT-046");
                        rejects += 1;
                    }
                }
            }
            assert_eq!(wins, 1, "round {round}: at most 1 producer may win");
            assert_eq!(rejects, TIGHT_THREADS - 1);
            let occ_after =
                measure_capture_queue_occupancy(tight_repo.path()).expect("tight occ after round");
            assert!(
                occ_after.total_bytes <= round_cap,
                "round {round}: total {} exceeded cap {}",
                occ_after.total_bytes,
                round_cap
            );
            assert_eq!(occ_after.total_bytes, occ_now.total_bytes + frame_len);
        }

        // ------------------------------------------------------------------
        // Part 3 — Corrupt sibling metadata conservative accounting &
        // security fail-closed checks (symlink, non-regular, reclaimed-name
        // hostile entries, and Unix foreign UID) with zero incoming writes
        // ------------------------------------------------------------------
        // 3a. Corrupt sibling metadata (`BothInvalidWithPresence`) MUST
        // conservatively count ALL sibling `events-*.log` files (`min_live=0`),
        // allowing healthy opposite-lane appends when under cap and rejecting
        // with `LBR-AGENT-046` + zero disk mutation when over cap.
        let sib_repo = tempdir().expect("corrupt sibling tempdir");
        let sib_init = init_capture_queue(sib_repo.path()).expect("init corrupt sibling queue");
        let sib_f0 = encode_frame(FRAME_FLAG_NONE, "sib-s0", &vec![b'A'; 100]).unwrap();
        let sib_f1 = encode_frame(FRAME_FLAG_NONE, "sib-s1", &vec![b'B'; 140]).unwrap();
        write_bytes_0600(
            &sib_init.paths.normal.join(segment_log_file_name(0)),
            &sib_f0,
        );
        write_bytes_0600(
            &sib_init.paths.normal.join(segment_log_file_name(1)),
            &sib_f1,
        );
        write_bytes_0600(&tail_meta_path(&sib_init.paths.normal), b"corrupt-primary");
        write_bytes_0600(
            &tail_meta_bak_path(&sib_init.paths.normal),
            b"corrupt-backup",
        );

        let sib_total = (sib_f0.len() + sib_f1.len()) as u64;
        let sib_occ =
            measure_capture_queue_occupancy(sib_repo.path()).expect("measure corrupt sibling occ");
        assert_eq!(
            sib_occ.normal_bytes, sib_total,
            "corrupt sibling metadata must conservatively count all sibling segment files"
        );
        let probe_prio = encode_frame(FRAME_FLAG_PRIORITY, "prio-probe", b"payload-12345").unwrap();
        let snap_corrupt_sib = snapshot_tree(&sib_init.paths.root);
        let tight_reject_opts = QueueAppendOptions::default()
            .with_max_total_bytes(sib_total + probe_prio.len() as u64 - 1);
        let sib_cap_err = append_encoded_frame_with_options(
            sib_repo.path(),
            CaptureQueueLane::Priority,
            &probe_prio,
            tight_reject_opts,
        )
        .expect_err("corrupt sibling bytes must enforce cap on healthy opposite lane");
        assert!(matches!(sib_cap_err, StorageBackendError::QueueFull { .. }));
        assert_eq!(sib_cap_err.stable_code_str(), "LBR-AGENT-046");
        assert_eq!(snapshot_tree(&sib_init.paths.root), snap_corrupt_sib);

        let exact_fit_opts =
            QueueAppendOptions::default().with_max_total_bytes(sib_total + probe_prio.len() as u64);
        let sib_ok = append_encoded_frame_with_options(
            sib_repo.path(),
            CaptureQueueLane::Priority,
            &probe_prio,
            exact_fit_opts,
        )
        .expect("healthy priority lane must still append when within conservative cap");
        assert_eq!(sib_ok.frame_len, probe_prio.len());

        // 3b. Non-regular directories at BOTH reclaimed (`seg0 < min_live_seg = 1`)
        // AND live (`seg2 >= min_live_seg = 1`) sibling segment names, as well as
        // inside `quarantine/`, MUST fail closed on opposite-lane append with zero
        // incoming writes. (On Windows, `CreateFileW` without
        // `FILE_FLAG_BACKUP_SEMANTICS` rejects directories with `Io` before
        // handle-attribute inspection, so match platform-appropriate fail-closed
        // error variants while strictly asserting zero disk mutation.)
        let sec_repo = tempdir().expect("security scan tempdir");
        let sec_init = init_capture_queue(sec_repo.path()).expect("init security scan queue");
        let sec_live = encode_frame(FRAME_FLAG_NONE, "sec-live-s1", b"valid-live").unwrap();
        let sec_s1_path = sec_init.paths.normal.join(segment_log_file_name(1));
        write_bytes_0600(&sec_s1_path, &sec_live);
        let sec_meta = LaneTailMeta {
            min_live_seg: 1,
            active_seg: 1,
            committed_len: sec_live.len() as u64,
            prefix_crc32c: crc32c_checksum(&sec_live),
            sealed: Vec::new(),
        };
        let sec_meta_bytes = sec_meta.encode().unwrap();
        write_bytes_0600(&tail_meta_path(&sec_init.paths.normal), &sec_meta_bytes);
        write_bytes_0600(&tail_meta_bak_path(&sec_init.paths.normal), &sec_meta_bytes);

        let reclaimed_s0_path = sec_init.paths.normal.join(segment_log_file_name(0));
        fs::create_dir(&reclaimed_s0_path).expect("plant dir at reclaimed seg0");
        let snap_reclaimed_dir = snapshot_tree(&sec_init.paths.root);
        let err_reclaimed_dir = append_frame(
            sec_repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "sec-reclaimed-dir",
            b"must-not-write",
        )
        .expect_err("directory at reclaimed sibling segment name must fail closed");
        #[cfg(unix)]
        assert!(matches!(
            err_reclaimed_dir,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        #[cfg(not(unix))]
        assert!(matches!(
            err_reclaimed_dir,
            StorageBackendError::CorruptCommittedFrame { .. } | StorageBackendError::Io { .. }
        ));
        assert_eq!(snapshot_tree(&sec_init.paths.root), snap_reclaimed_dir);
        fs::remove_dir(&reclaimed_s0_path).expect("remove dir at reclaimed seg0");

        // Non-regular directory at LIVE sibling segment name (`seg2 >= min_live_seg = 1`).
        let live_s2_dir_path = sec_init.paths.normal.join(segment_log_file_name(2));
        fs::create_dir(&live_s2_dir_path).expect("plant dir at live seg2");
        let snap_live_dir = snapshot_tree(&sec_init.paths.root);
        let err_live_dir = append_frame(
            sec_repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "sec-live-dir",
            b"must-not-write",
        )
        .expect_err("directory at live sibling segment name must fail closed");
        #[cfg(unix)]
        assert!(matches!(
            err_live_dir,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        #[cfg(not(unix))]
        assert!(matches!(
            err_live_dir,
            StorageBackendError::CorruptCommittedFrame { .. } | StorageBackendError::Io { .. }
        ));
        assert_eq!(snapshot_tree(&sec_init.paths.root), snap_live_dir);
        fs::remove_dir(&live_s2_dir_path).expect("remove dir at live seg2");

        // Non-regular directory in `quarantine/` must fail closed with zero writes.
        let q_dir_entry = sec_init.paths.quarantine.join("not-a-regular-file.bin");
        fs::create_dir(&q_dir_entry).expect("plant dir in quarantine");
        let snap_q_dir = snapshot_tree(&sec_init.paths.root);
        let err_q_dir = append_frame(
            sec_repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "sec-q-dir",
            b"must-not-write",
        )
        .expect_err("directory inside quarantine must fail closed");
        #[cfg(unix)]
        assert!(matches!(
            err_q_dir,
            StorageBackendError::BackendInternal { .. }
        ));
        #[cfg(not(unix))]
        assert!(matches!(
            err_q_dir,
            StorageBackendError::BackendInternal { .. }
                | StorageBackendError::CorruptCommittedFrame { .. }
                | StorageBackendError::Io { .. }
        ));
        assert_eq!(snapshot_tree(&sec_init.paths.root), snap_q_dir);
        fs::remove_dir(&q_dir_entry).expect("remove dir in quarantine");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let decoy_file = sec_repo.path().join("decoy-target");
            write_bytes_0600(&decoy_file, b"decoy");

            // Symlink at RECLAIMED sibling segment name (`seg0 < min_live_seg=1`).
            symlink(&decoy_file, &reclaimed_s0_path).expect("plant symlink at reclaimed seg0");
            let snap_reclaimed_sym = snapshot_tree(&sec_init.paths.root);
            let err_reclaimed_sym = append_frame(
                sec_repo.path(),
                CaptureQueueLane::Priority,
                FRAME_FLAG_PRIORITY,
                "sec-reclaimed-sym",
                b"must-not-write",
            )
            .expect_err("symlink at reclaimed sibling segment name must fail closed");
            assert!(
                crate::storage::queue::is_capture_queue_symlink_rejected_error(&err_reclaimed_sym),
                "expected symlink rejection for reclaimed seg0, got {err_reclaimed_sym:?}"
            );
            assert_eq!(snapshot_tree(&sec_init.paths.root), snap_reclaimed_sym);
            fs::remove_file(&reclaimed_s0_path).expect("remove reclaimed symlink");

            // Symlink at LIVE sibling segment name (`seg2 >= min_live_seg=1`).
            let live_sym_path = sec_init.paths.normal.join(segment_log_file_name(2));
            symlink(&decoy_file, &live_sym_path).expect("plant symlink at live seg2");
            let snap_live_sym = snapshot_tree(&sec_init.paths.root);
            let err_live_sym = append_frame(
                sec_repo.path(),
                CaptureQueueLane::Priority,
                FRAME_FLAG_PRIORITY,
                "sec-live-sym",
                b"must-not-write",
            )
            .expect_err("symlink at live sibling segment name must fail closed");
            assert!(
                crate::storage::queue::is_capture_queue_symlink_rejected_error(&err_live_sym),
                "expected symlink rejection for live seg2, got {err_live_sym:?}"
            );
            assert_eq!(snapshot_tree(&sec_init.paths.root), snap_live_sym);
            fs::remove_file(&live_sym_path).expect("remove live symlink");

            // Symlink inside `quarantine/`.
            let q_sym_path = sec_init.paths.quarantine.join("symlink-entry.bin");
            symlink(&decoy_file, &q_sym_path).expect("plant symlink in quarantine");
            let snap_q_sym = snapshot_tree(&sec_init.paths.root);
            let err_q_sym = append_frame(
                sec_repo.path(),
                CaptureQueueLane::Priority,
                FRAME_FLAG_PRIORITY,
                "sec-q-sym",
                b"must-not-write",
            )
            .expect_err("symlink inside quarantine must fail closed");
            assert!(
                crate::storage::queue::is_capture_queue_symlink_rejected_error(&err_q_sym),
                "expected symlink rejection for quarantine entry, got {err_q_sym:?}"
            );
            assert_eq!(snapshot_tree(&sec_init.paths.root), snap_q_sym);
            fs::remove_file(&q_sym_path).expect("remove quarantine symlink");

            // Foreign-UID checks via both full `append_in_storage_dir_unix`
            // (mismatched sibling owner) and ISOLATED file-level scans covering
            // (1) live-only segment (`seg1` alone, zero reclaimed files),
            // (2) reclaimed-only segment (`seg0` alone, zero live files), and
            // (3) quarantine entry.
            write_bytes_0600(
                &sec_init.paths.quarantine.join("foreign-uid-entry.bin"),
                b"q-bytes",
            );
            let real_owners = UnixExpectedQueueOwners::current_effective();
            let wrong_uid = real_owners.root_uid.wrapping_add(1);

            let mut wrong_normal_owners = real_owners;
            wrong_normal_owners.normal_uid = wrong_uid;
            let snap_uid = snapshot_tree(&sec_init.paths.root);
            let mut uid_obs = SegmentObserver::new();
            let err_uid_append = unix_seg::append_in_storage_dir_unix(
                sec_repo.path(),
                CaptureQueueLane::Priority,
                &probe_prio,
                QueueAppendOptions::default(),
                wrong_normal_owners,
                &mut uid_obs,
            )
            .expect_err("foreign sibling UID must fail closed with zero incoming writes");
            assert!(
                crate::storage::queue::is_capture_queue_owner_mismatch_error(&err_uid_append),
                "expected owner mismatch error, got {err_uid_append:?}"
            );
            assert_eq!(snapshot_tree(&sec_init.paths.root), snap_uid);

            let (normal_fd, _) = open_dir_nofollow(
                &sec_init.paths.normal,
                real_owners.normal_uid,
                "open normal lane for uid scan test",
            )
            .expect("open normal_fd");

            // (1) Isolated live-only foreign-UID check (`seg1` present, `seg0` absent).
            assert!(!reclaimed_s0_path.exists() && sec_s1_path.exists());
            let err_live_uid =
                unix_seg::measure_lane_unconsumed_bytes_unix(&normal_fd, 1, wrong_uid)
                    .expect_err("foreign UID on isolated live segment must fail closed");
            assert!(crate::storage::queue::is_capture_queue_owner_mismatch_error(&err_live_uid));

            // (2) Isolated reclaimed-only foreign-UID check (`seg0` present, `seg1` absent).
            fs::remove_file(&sec_s1_path)
                .expect("remove seg1 for isolated reclaimed-only uid test");
            write_bytes_0600(&reclaimed_s0_path, b"reclaimed-regular-bytes");
            assert!(reclaimed_s0_path.exists() && !sec_s1_path.exists());
            let err_reclaimed_uid =
                unix_seg::measure_lane_unconsumed_bytes_unix(&normal_fd, 1, wrong_uid)
                    .expect_err("foreign UID on isolated reclaimed segment must fail closed");
            assert!(
                crate::storage::queue::is_capture_queue_owner_mismatch_error(&err_reclaimed_uid)
            );

            // (3) Isolated quarantine entry foreign-UID check.
            let (q_fd, _) = open_dir_nofollow(
                &sec_init.paths.quarantine,
                real_owners.quarantine_uid,
                "open quarantine for uid scan test",
            )
            .expect("open q_fd");
            let err_q_uid = unix_seg::measure_quarantine_bytes_unix(&q_fd, wrong_uid)
                .expect_err("foreign UID on quarantine entry must fail closed");
            assert!(crate::storage::queue::is_capture_queue_owner_mismatch_error(&err_q_uid));
        }
    }

    /// `RB-08b` AC3 + AC4:
    /// - AC3: Holding shared `append.lock` beyond caller's deadline returns
    ///   `LBR-AGENT-049` (`StorageBackendError::QueueBusy`) with zero bytes
    ///   written and zero disk mutation; releasing the lock allows immediate
    ///   subsequent appends on both lanes; timeout > 30s clamps to 30s.
    /// - AC4: 1 KiB sanitized frame enqueue (including Case A hardware
    ///   `PREFIX_CRC32C` verification and dual-replica `tail.meta` +
    ///   `tail.meta.bak` atomic commit) achieves median latency `<= 5ms` (hard
    ///   budget ceiling `<= 50ms`) on local SSD.
    #[test]
    fn append_lock_deadline_timeout_and_latency_budget() {
        // ------------------------------------------------------------------
        // Part 1 — AC3: Shared `append.lock` deadline timeout (`LBR-AGENT-049`)
        // ------------------------------------------------------------------
        let clamped_timeout = QueueAppendOptions::default()
            .with_lock_timeout(Duration::from_secs(120))
            .effective_lock_timeout();
        assert_eq!(clamped_timeout, DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT);

        let repo = tempdir().expect("lock timeout tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let _seed = append_frame(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb08b-ac3-seed",
            b"seed-before-lock-contention",
        )
        .expect("append seed before lock test");
        let snap_before_lock = snapshot_tree(&init.paths.root);

        let lock_path = init.paths.root.join(QUEUE_APPEND_LOCK_FILE);
        let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let lock_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("holder open append.lock");
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd as _;
                // SAFETY: `lock_file` is a valid open descriptor.
                let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
                assert_eq!(rc, 0, "holder failed to flock append.lock");
            }
            #[cfg(not(unix))]
            {
                lock_file.lock().expect("holder failed to lock append.lock");
            }
            locked_tx.send(()).expect("signal lock acquired");
            release_rx.recv().expect("wait for release signal");
            drop(lock_file);
        });

        locked_rx.recv().expect("holder acquired append.lock");

        // Contended Priority append with 80ms deadline -> LBR-AGENT-049 + zero writes.
        let timeout_80ms =
            QueueAppendOptions::default().with_lock_timeout(Duration::from_millis(80));
        let t0 = Instant::now();
        let err_prio = append_frame_with_options(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb08b-ac3-blocked-prio",
            b"must-not-be-written",
            timeout_80ms,
        )
        .expect_err("contended append past 80ms deadline must fail with LBR-AGENT-049");
        let elapsed_80ms = t0.elapsed();
        assert!(matches!(err_prio, StorageBackendError::QueueBusy { .. }));
        assert_eq!(err_prio.stable_code_str(), "LBR-AGENT-049");
        assert!(
            elapsed_80ms >= Duration::from_millis(60),
            "expected wait near 80ms deadline, got {elapsed_80ms:?}"
        );
        assert!(
            elapsed_80ms < Duration::from_secs(2),
            "wait far exceeded 80ms deadline: {elapsed_80ms:?}"
        );
        assert_eq!(
            snapshot_tree(&init.paths.root),
            snap_before_lock,
            "AC3 violated: timed-out append must write zero bytes"
        );

        // Contended Normal append with zero deadline -> immediate LBR-AGENT-049 + zero writes.
        let timeout_zero = QueueAppendOptions::default().with_lock_timeout(Duration::ZERO);
        let t_zero = Instant::now();
        let err_norm = append_frame_with_options(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08b-ac3-blocked-norm",
            b"must-not-create-seg0",
            timeout_zero,
        )
        .expect_err("zero-deadline contended append must fail immediately");
        let elapsed_zero = t_zero.elapsed();
        assert!(matches!(err_norm, StorageBackendError::QueueBusy { .. }));
        assert_eq!(err_norm.stable_code_str(), "LBR-AGENT-049");
        assert!(
            elapsed_zero < Duration::from_millis(500),
            "zero-deadline wait took unexpectedly long: {elapsed_zero:?}"
        );
        assert!(!init.paths.normal.join(segment_log_file_name(0)).exists());
        assert_eq!(snapshot_tree(&init.paths.root), snap_before_lock);

        // Release holder and confirm immediate recovery on both lanes.
        release_tx.send(()).expect("signal holder release");
        holder.join().expect("join lock holder thread");

        // Deterministic clock/lock regression test (shared by both Unix & Windows
        // retry loops): prove contention occurred on attempt 1, then advance the
        // virtual clock strictly past `deadline` AND make the lock available
        // during sleep. The pre-retry deadline check MUST reject with
        // `LBR-AGENT-049` without invoking `try_lock` a second time (removing
        // the pre-retry check deterministically breaks this test by acquiring!).
        {
            use std::cell::Cell;

            let t0 = Instant::now();
            let deadline = t0 + Duration::from_millis(10);
            let clock = Cell::new(t0);
            let lock_available = Cell::new(false);
            let attempts = Cell::new(0usize);
            let sleeps = Cell::new(0usize);

            let post_err = acquire_lock_with_deadline_loop(
                deadline,
                "deterministic post-deadline test",
                || {
                    attempts.set(attempts.get() + 1);
                    if lock_available.get() {
                        TryLockOutcome::Acquired
                    } else {
                        TryLockOutcome::WouldBlock
                    }
                },
                || clock.get(),
                |_dur| {
                    sleeps.set(sleeps.get() + 1);
                    // Advance past deadline AND release the lock before retry.
                    clock.set(deadline + Duration::from_millis(1));
                    lock_available.set(true);
                },
            )
            .expect_err("lock released after deadline MUST NOT be acquired on retry");
            assert!(matches!(post_err, StorageBackendError::QueueBusy { .. }));
            assert_eq!(post_err.stable_code_str(), "LBR-AGENT-049");
            assert_eq!(attempts.get(), 1, "must NOT re-attempt lock after deadline");
            assert_eq!(sleeps.get(), 1, "contention sleep must have occurred");

            // Control: when lock becomes available BEFORE deadline, retry acquires.
            let clock_ok = Cell::new(t0);
            let avail_ok = Cell::new(false);
            let attempts_ok = Cell::new(0usize);
            acquire_lock_with_deadline_loop(
                deadline,
                "deterministic pre-deadline acquire test",
                || {
                    attempts_ok.set(attempts_ok.get() + 1);
                    if avail_ok.get() {
                        TryLockOutcome::Acquired
                    } else {
                        TryLockOutcome::WouldBlock
                    }
                },
                || clock_ok.get(),
                |dur| {
                    clock_ok.set(clock_ok.get() + dur);
                    avail_ok.set(true);
                },
            )
            .expect("lock released before deadline must be acquired on retry");
            assert_eq!(attempts_ok.get(), 2);

            // Control: `Duration::ZERO` (`deadline == t0`) still tries once immediately.
            let zero_attempts = Cell::new(0usize);
            acquire_lock_with_deadline_loop(
                t0,
                "zero-deadline uncontended test",
                || {
                    zero_attempts.set(zero_attempts.get() + 1);
                    TryLockOutcome::Acquired
                },
                || t0,
                |_| panic!("uncontended zero-deadline attempt must not sleep"),
            )
            .expect("uncontended zero-deadline attempt must succeed");
            assert_eq!(zero_attempts.get(), 1);
        }

        let recovered_p = append_frame_with_options(
            repo.path(),
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb08b-ac3-after-release-p",
            b"ok-after-release",
            timeout_80ms,
        )
        .expect("priority append must succeed after lock release");
        let recovered_n = append_frame_with_options(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08b-ac3-after-release-n",
            b"ok-after-release-normal",
            timeout_80ms,
        )
        .expect("normal append must succeed after lock release");
        assert_eq!(recovered_p.seg_id, 0);
        assert_eq!(recovered_n.seg_id, 0);

        // ------------------------------------------------------------------
        // Part 2 — AC4: 1 KiB sanitized frame enqueue latency budget on SSD
        // (includes Case A hardware PREFIX_CRC32C + dual-replica commit)
        // ------------------------------------------------------------------
        let perf_repo = tempdir().expect("latency budget tempdir");
        let perf_init = init_capture_queue(perf_repo.path()).expect("init latency queue");

        // Construct exact 1024-byte sanitized LCE1 frames:
        // total = 16 (header) + 3 (flags + key_len) + key.len() + payload.len()
        let key_prefix = "rb08b-lat-";
        let key_len = key_prefix.len() + 4; // e.g., "rb08b-lat-0001" = 14 bytes
        let payload_len = 1024 - FRAME_HEADER_LEN - FRAME_BODY_FIXED_OVERHEAD - key_len;
        let sanitized_payload = vec![b'S'; payload_len];
        let sample_encoded = encode_frame(
            FRAME_FLAG_NONE,
            &format!("{key_prefix}0000"),
            &sanitized_payload,
        )
        .expect("encode 1 KiB check frame");
        assert_eq!(sample_encoded.len(), 1024);

        // Warmup 3 frames so `events-00000000.log` exists and every measured
        // call traverses the full Case A `PREFIX_CRC32C` verification path.
        for i in 0..3usize {
            let key = format!("{key_prefix}w{i:03}");
            let out = append_frame(
                perf_repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &key,
                &sanitized_payload,
            )
            .expect("warmup append");
            assert_eq!(out.frame_len, 1024);
        }

        const MEASURED_ITERS: usize = 31;
        let mut samples = Vec::with_capacity(MEASURED_ITERS);
        for i in 0..MEASURED_ITERS {
            let key = format!("{key_prefix}{i:04}");
            let start = Instant::now();
            let out = append_frame(
                perf_repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &key,
                &sanitized_payload,
            )
            .expect("measured 1 KiB append");
            samples.push(start.elapsed());
            assert_eq!(out.frame_len, 1024);
        }

        samples.sort_unstable();
        let median = samples[MEASURED_ITERS / 2];
        let max_sample = samples[MEASURED_ITERS - 1];
        // Note: on macOS APFS, Rust's `File::sync_data()` issues hardware
        // `fcntl(F_FULLFSYNC)` (~4.15–6.10 ms per flush × 3 durable barriers
        // per append ≈ 16–20 ms), which MUST NOT be relaxed to `F_BARRIERFSYNC`
        // (per `man 2 fcntl`, `F_BARRIERFSYNC` does not guarantee durability on
        // return). Enforce the AC4 `<= 50ms` hard ceiling on BOTH median and
        // max across all platforms, enforce the `<= 5ms` full-enqueue median
        // target on non-macOS SSDs (`fdatasync`/`FlushFileBuffers`), AND verify
        // on all platforms that the Case A `PREFIX_CRC32C` + cross-lane
        // capacity check path stays strictly `<= 5ms`.
        #[cfg(not(target_os = "macos"))]
        assert!(
            median <= Duration::from_millis(5),
            "AC4 violated: 1 KiB Case A durable enqueue median latency {:?} exceeded 5ms target (max={:?})",
            median,
            max_sample
        );
        assert!(
            median <= Duration::from_millis(50),
            "AC4 violated: 1 KiB Case A durable enqueue median latency {:?} exceeded 50ms hard budget ceiling (max={:?})",
            median,
            max_sample
        );
        assert!(
            max_sample <= Duration::from_millis(50),
            "AC4 violated: 1 KiB Case A durable enqueue max latency {:?} exceeded 50ms hard budget ceiling (median={:?})",
            max_sample,
            median
        );

        // Verify all 34 (3 warmup + 31 measured) frames decoded cleanly with matching dual LTM2.
        let perf_meta = read_converged_lane_tail_meta(&perf_init.paths.normal)
            .expect("read latency lane tail.meta");
        assert_eq!(perf_meta.committed_len, (3 + MEASURED_ITERS) as u64 * 1024);
        assert_eq!(
            fs::read(tail_meta_path(&perf_init.paths.normal)).unwrap(),
            fs::read(tail_meta_bak_path(&perf_init.paths.normal)).unwrap()
        );
        let seg_bytes = fs::read(perf_init.paths.normal.join(segment_log_file_name(0))).unwrap();
        assert_eq!(crc32c_checksum(&seg_bytes), perf_meta.prefix_crc32c);

        // Directly verify that the Case A hardware `PREFIX_CRC32C` + cross-lane
        // occupancy check path itself stays well within the 5ms median target.
        let seg_path = perf_init.paths.normal.join(segment_log_file_name(0));
        let mut check_samples = Vec::with_capacity(MEASURED_ITERS);
        for _ in 0..MEASURED_ITERS {
            let start = Instant::now();
            let mut seg_ro = File::open(&seg_path).expect("open perf seg readonly");
            verify_committed_prefix_crc(
                &mut seg_ro,
                perf_meta.committed_len,
                perf_meta.prefix_crc32c,
            )
            .expect("Case A PREFIX_CRC32C check must pass");
            let occ =
                measure_capture_queue_occupancy(perf_repo.path()).expect("measure perf occupancy");
            assert_eq!(occ.normal_bytes, perf_meta.committed_len);
            check_samples.push(start.elapsed());
        }
        check_samples.sort_unstable();
        let check_median = check_samples[MEASURED_ITERS / 2];
        assert!(
            check_median <= Duration::from_millis(5),
            "AC4 violated: Case A PREFIX_CRC32C + cross-lane occupancy median {:?} exceeded 5ms",
            check_median
        );
    }

    fn decode_all_segment_bytes(seg_bytes: &[u8]) -> Vec<DecodedFrame> {
        let mut decoder = FrameStreamDecoder::new(seg_bytes, seg_bytes.len() as u64);
        let mut out = Vec::new();
        loop {
            match decoder.next_frame().expect("decode segment frame") {
                FrameStreamItem::Frame { frame, .. } => out.push(frame),
                FrameStreamItem::CleanEof { .. } => break,
            }
        }
        out
    }

    fn make_exact_len_frame(
        flags: u8,
        dedup_key: &str,
        total_len: usize,
        fill_byte: u8,
    ) -> (Vec<u8>, Vec<u8>) {
        let overhead = FRAME_HEADER_LEN + FRAME_BODY_FIXED_OVERHEAD + dedup_key.len();
        assert!(
            total_len >= overhead,
            "requested total_len {total_len} < frame overhead {overhead}"
        );
        let payload = vec![fill_byte; total_len - overhead];
        let encoded = encode_frame(flags, dedup_key, &payload).expect("encode exact-len frame");
        assert_eq!(encoded.len(), total_len);
        (payload, encoded)
    }

    /// `RB-08g` AC1–AC8:
    /// - AC1: Crossing `CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES` (`16 MiB`) calls
    ///   shared `create_segment_file` to create `events-<S+1:08>.log` on disk.
    /// - AC2: Newly created `events-<S+1:08>.log` has exact `0600` Unix mode.
    /// - AC3: `create_segment_file` immediately executes `fsync_dir(lane_dir)`.
    /// - AC4: Dual-replica `LTM2` commit atomically records
    ///   `(S, old_committed_len, old_prefix_crc32c)` into `LTM2.sealed[]`.
    /// - AC5: Same dual-replica `LTM2` commit advances `ACTIVE_SEG` to `S + 1`.
    /// - AC6: New segment `PREFIX_CRC32C` resets from `0` for `S + 1` first
    ///   frame and matches `events-<S+1>[0..committed_len]`.
    /// - AC7: All old frames in sealed segment `S` decode 100% losslessly.
    /// - AC8: All new frames in rotated segment `S + 1` decode 100% losslessly.
    /// Also verifies exact `== 16 MiB` stays in `S`, `< 16 MiB` crossing `16 MiB`
    /// seals at exact `< 16 MiB` boundary, standalone `create_segment_file`
    /// invariants, and post-rotation `AfterSegmentSyncData` crash recovery.
    #[test]
    fn rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed() {
        assert_eq!(CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES, 16 * 1024 * 1024);

        // ------------------------------------------------------------------
        // Part 1 — Exact 16 MiB boundary (== 16 MiB stays in seg 0; next frame
        // rotates to seg 1 with full AC1–AC8 assertions)
        // ------------------------------------------------------------------
        let repo = tempdir().expect("rotation tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");

        // 4 × (4 MiB - 1024) = 16 MiB - 4096 bytes, then 1 × 4096 bytes = exact 16 MiB.
        const CHUNK_FRAME_LEN: usize = 4 * 1024 * 1024 - 1024;
        let mut expected_seg0_keys = Vec::new();
        let mut expected_seg0_payload_lens = Vec::new();
        let mut expected_seg0_fill = Vec::new();
        let mut last_seg0_out = None;

        for idx in 0..4usize {
            let key = format!("rb08g-s0-big-{idx:02}");
            let fill = b'A' + (idx as u8);
            let (payload, encoded) =
                make_exact_len_frame(FRAME_FLAG_NONE, &key, CHUNK_FRAME_LEN, fill);
            let out = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &encoded)
                .expect("append large seg0 frame");
            assert_eq!(out.seg_id, 0);
            assert!(out.meta.sealed.is_empty());
            expected_seg0_keys.push(key);
            expected_seg0_payload_lens.push(payload.len());
            expected_seg0_fill.push(fill);
            last_seg0_out = Some(out);
        }

        // Frame 4 lands at EXACT `16 MiB` (`16_777_216`) -> MUST NOT rotate.
        let exact_key = "rb08g-s0-exact-16mib".to_string();
        let (exact_payload, exact_encoded) =
            make_exact_len_frame(FRAME_FLAG_NONE, &exact_key, 4096, b'E');
        let exact_out = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &exact_encoded)
            .expect("exact 16 MiB frame must remain in seg 0");
        assert_eq!(exact_out.seg_id, 0);
        assert_eq!(
            exact_out.meta.committed_len,
            CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES
        );
        assert!(exact_out.meta.sealed.is_empty());
        assert!(
            !init.paths.normal.join(segment_log_file_name(1)).exists(),
            "exact == 16 MiB must NOT create seg 1 prematurely"
        );
        expected_seg0_keys.push(exact_key);
        expected_seg0_payload_lens.push(exact_payload.len());
        expected_seg0_fill.push(b'E');
        let _ = last_seg0_out;
        let old_seg0_len = exact_out.meta.committed_len;
        let old_seg0_crc = exact_out.meta.prefix_crc32c;

        // Frame 5 crosses 16 MiB -> triggers shared `create_segment_file` rotation!
        let rot_key_1 = "rb08g-s1-first".to_string();
        let (rot_payload_1, rot_encoded_1) =
            make_exact_len_frame(FRAME_FLAG_NONE, &rot_key_1, 512, b'R');
        let mut rot_obs = SegmentObserver::new();
        let rot_out = append_validated_frame_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &rot_encoded_1,
            &mut rot_obs,
        )
        .expect("frame crossing 16 MiB must rotate to seg 1");

        // AC1: `events-00000001.log` exists on disk.
        let seg0_path = init.paths.normal.join(segment_log_file_name(0));
        let seg1_path = init.paths.normal.join(segment_log_file_name(1));
        assert!(seg1_path.exists(), "AC1 violated: seg 1 must exist on disk");

        // AC2: exact `0600` permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::symlink_metadata(&seg1_path)
                .expect("stat seg 1")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o600, "AC2 violated: seg 1 mode must be exact 0600");
        }

        // AC3: exact ordering proves `CreateSegment(1) -> FsyncLaneDirAfterCreateSegment(1)`
        // strictly precedes `SegmentWriteAll(1)` and dual `LTM2` commits.
        assert_eq!(
            rot_obs.events,
            vec![
                SegmentOpEvent::AcquireAppendLock,
                SegmentOpEvent::CapacityChecked {
                    priority_bytes: 0,
                    normal_bytes: old_seg0_len,
                    quarantine_bytes: 0,
                    incoming_bytes: rot_encoded_1.len(),
                    max_total_bytes: CAPTURE_QUEUE_MAX_TOTAL_BYTES,
                },
                SegmentOpEvent::CreateSegment {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                },
                SegmentOpEvent::FsyncLaneDirAfterCreateSegment {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                },
                SegmentOpEvent::SegmentWriteAll {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                    offset: 0,
                    len: rot_encoded_1.len(),
                },
                SegmentOpEvent::SegmentSyncData {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                },
                SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::MetaRename {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_FILE,
                },
                SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::MetaRename {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_BAK_TMP_FILE,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                },
                SegmentOpEvent::ReleaseAppendLock,
            ],
            "AC3 violated: create_segment_file must fsync_dir(lane_dir) immediately before write/meta commit"
        );

        // AC4, AC5, AC6: verify both on-disk `tail.meta` and `tail.meta.bak`.
        let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).expect("read tail.meta");
        let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).expect("read tail.meta.bak");
        assert_eq!(raw_meta, raw_bak);
        let decoded_meta = LaneTailMeta::decode(&raw_meta).expect("decode rotated LTM2");
        assert_eq!(decoded_meta, rot_out.meta);
        assert_eq!(
            decoded_meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: old_seg0_len,
                sealed_crc32c: old_seg0_crc,
            }],
            "AC4 violated: old seg 0 must be sealed in LTM2.sealed[]"
        );
        assert_eq!(
            decoded_meta.active_seg, 1,
            "AC5 violated: ACTIVE_SEG must advance to 1"
        );
        assert_eq!(rot_out.seg_id, 1);
        assert_eq!(rot_out.frame_offset, 0);
        assert_eq!(decoded_meta.committed_len, rot_encoded_1.len() as u64);
        assert_eq!(
            decoded_meta.prefix_crc32c,
            crc32c_checksum(&rot_encoded_1),
            "AC6 violated: new segment PREFIX_CRC32C must reset from 0"
        );
        assert_ne!(
            decoded_meta.prefix_crc32c,
            crc32c_append(old_seg0_crc, &rot_encoded_1),
            "AC6 sanity: new segment CRC must not chain from old segment"
        );
        let seg1_disk_after_first = fs::read(&seg1_path).expect("read seg 1 after first frame");
        assert_eq!(
            crc32c_checksum(&seg1_disk_after_first),
            decoded_meta.prefix_crc32c
        );

        // Append a second frame to rotated segment 1 (verifying subsequent Case A append).
        let rot_key_2 = "rb08g-s1-second".to_string();
        let (rot_payload_2, rot_encoded_2) =
            make_exact_len_frame(FRAME_FLAG_NONE, &rot_key_2, 768, b'S');
        let rot_out_2 = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &rot_encoded_2)
            .expect("second frame on rotated seg 1 must succeed");
        assert_eq!(rot_out_2.seg_id, 1);
        assert_eq!(rot_out_2.frame_offset, rot_encoded_1.len() as u64);
        assert_eq!(rot_out_2.meta.sealed, decoded_meta.sealed);
        let seg1_bytes = fs::read(&seg1_path).expect("read seg 1 bytes");
        assert_eq!(seg1_bytes.len() as u64, rot_out_2.meta.committed_len);
        assert_eq!(crc32c_checksum(&seg1_bytes), rot_out_2.meta.prefix_crc32c);

        // AC7: 100% lossless decode of ALL 5 frames in sealed segment 0.
        let seg0_bytes = fs::read(&seg0_path).expect("read seg 0 bytes");
        assert_eq!(seg0_bytes.len() as u64, old_seg0_len);
        assert_eq!(crc32c_checksum(&seg0_bytes), old_seg0_crc);
        let decoded_seg0 = decode_all_segment_bytes(&seg0_bytes);
        assert_eq!(decoded_seg0.len(), expected_seg0_keys.len());
        for (idx, frame) in decoded_seg0.iter().enumerate() {
            assert_eq!(frame.flags, FRAME_FLAG_NONE);
            assert_eq!(frame.dedup_key, expected_seg0_keys[idx]);
            assert_eq!(frame.payload.len(), expected_seg0_payload_lens[idx]);
            assert!(frame.payload.iter().all(|&b| b == expected_seg0_fill[idx]));
        }

        // AC8: 100% lossless decode of ALL 2 frames in rotated segment 1.
        let decoded_seg1 = decode_all_segment_bytes(&seg1_bytes);
        assert_eq!(decoded_seg1.len(), 2);
        assert_eq!(decoded_seg1[0].dedup_key, rot_key_1);
        assert_eq!(decoded_seg1[0].payload, rot_payload_1);
        assert_eq!(decoded_seg1[1].dedup_key, rot_key_2);
        assert_eq!(decoded_seg1[1].payload, rot_payload_2);

        // ------------------------------------------------------------------
        // Part 2 — `< 16 MiB` crossing `16 MiB` seals at exact `< 16 MiB` len,
        // plus crash after rotated `seg.sync_data()` recovers cleanly via Case D
        // ------------------------------------------------------------------
        let repo2 = tempdir().expect("rotation under-16mib tempdir");
        let init2 = init_capture_queue(repo2.path()).expect("init capture queue 2");
        let mut s0_meta = None;
        for idx in 0..4usize {
            let key = format!("rb08g-p2-s0-{idx:02}");
            let (_, encoded) =
                make_exact_len_frame(FRAME_FLAG_PRIORITY, &key, CHUNK_FRAME_LEN, b'P');
            s0_meta = Some(
                append_encoded_frame(repo2.path(), CaptureQueueLane::Priority, &encoded)
                    .expect("append p2 seg0 frame")
                    .meta,
            );
        }
        let s0_meta = s0_meta.expect("s0_meta populated");
        assert_eq!(
            s0_meta.committed_len,
            CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES - 4096
        );

        // Frame of 4097 bytes -> `(16 MiB - 4096) + 4097 == 16 MiB + 1 > 16 MiB` -> rotates!
        // Inject crash right after `seg.sync_data()` on new seg 1 before `tail.meta` commit.
        let (_, cross_frame) =
            make_exact_len_frame(FRAME_FLAG_PRIORITY, "rb08g-p2-cross", 4097, b'C');
        let mut crash_obs = SegmentObserver::with_crash(SegmentCrashPoint::AfterSegmentSyncData);
        let crash_err = append_validated_frame_in_storage_dir(
            repo2.path(),
            CaptureQueueLane::Priority,
            &cross_frame,
            &mut crash_obs,
        )
        .expect_err("injected crash after rotated seg1 sync_data must fail");
        assert_eq!(crash_err.stable_code_str(), "LBR-IO-001");
        assert!(init2.paths.priority.join(segment_log_file_name(1)).exists());

        // Recovery (Case D valid successor) MUST seal seg 0 at `16 MiB - 4096`
        // and advance `LTM2` to seg 1 with `cross_frame` 100% preserved.
        let rec = recover_lane(repo2.path(), CaptureQueueLane::Priority)
            .expect("recover_lane after rotated seg1 crash must succeed");
        assert_eq!(rec.meta.active_seg, 1);
        assert_eq!(rec.meta.committed_len, cross_frame.len() as u64);
        assert_eq!(rec.meta.prefix_crc32c, crc32c_checksum(&cross_frame));
        assert_eq!(
            rec.meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES - 4096,
                sealed_crc32c: s0_meta.prefix_crc32c,
            }]
        );
        let rec_seg1_frames = decode_all_segment_bytes(
            &fs::read(init2.paths.priority.join(segment_log_file_name(1))).unwrap(),
        );
        assert_eq!(rec_seg1_frames.len(), 1);
        assert_eq!(rec_seg1_frames[0].dedup_key, "rb08g-p2-cross");

        // ------------------------------------------------------------------
        // Part 3 — Internal locked `create_segment_file` contract checks
        // (strictly confined inside `append.lock`, never leaking unlocked
        // handles or arbitrary noncontiguous segments to public callers)
        // ------------------------------------------------------------------
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let repo3 = tempdir().expect("create_segment_file tempdir");
            let init3 = init_capture_queue(repo3.path()).expect("init capture queue 3");
            let _seed = append_frame(
                repo3.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08g-p3-seed",
                b"seed-seg0",
            )
            .expect("seed seg 0");
            let owners = UnixExpectedQueueOwners::current_effective();
            let (root_fd, _) =
                open_dir_nofollow(&init3.paths.root, owners.root_uid, "test root dir")
                    .expect("open root_fd");
            let (lane_fd, _) =
                open_dir_nofollow(&init3.paths.normal, owners.normal_uid, "test normal dir")
                    .expect("open lane_fd");
            let append_lock = unix_seg::try_acquire_existing_strict_append_lock_unix(
                &root_fd,
                owners.root_uid,
                lock_deadline_for_timeout(DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT),
            )
            .expect("acquire append.lock")
            .expect("append.lock must exist after init_capture_queue");

            // Prove `append.lock` is genuinely held exclusively by verifying a
            // zero-timeout contending append fails closed with `LBR-AGENT-049`.
            let contended_err = append_frame_with_options(
                repo3.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08g-p3-contended",
                b"blocked-by-held-lock",
                QueueAppendOptions::default().with_lock_timeout(Duration::ZERO),
            )
            .expect_err("contending append while lock guard is held must fail with LBR-AGENT-049");
            assert!(matches!(
                contended_err,
                StorageBackendError::QueueBusy { .. }
            ));
            assert_eq!(contended_err.stable_code_str(), "LBR-AGENT-049");

            let mut direct_obs = SegmentObserver::new();
            let created_file = unix_seg::create_segment_file_at_fd_unix(
                &lane_fd,
                QUEUE_LANE_NORMAL_DIR,
                1,
                owners.normal_uid,
                &append_lock,
                &mut direct_obs,
            )
            .expect("locked create_segment_file_at_fd_unix for contiguous seg 1 must succeed");
            drop(created_file);

            let seg1_path3 = init3.paths.normal.join(segment_log_file_name(1));
            assert!(seg1_path3.exists());
            assert_eq!(fs::metadata(&seg1_path3).unwrap().len(), 0);
            let mode = fs::symlink_metadata(&seg1_path3)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o600);
            assert_eq!(
                direct_obs.events,
                vec![
                    SegmentOpEvent::CreateSegment {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 1,
                    },
                    SegmentOpEvent::FsyncLaneDirAfterCreateSegment {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 1,
                    },
                ]
            );
            // Duplicate `O_CREAT | O_EXCL` on existing seg 1 must fail with AlreadyExists.
            let mut dup_obs = SegmentObserver::new();
            let dup_err = unix_seg::create_segment_file_at_fd_unix(
                &lane_fd,
                QUEUE_LANE_NORMAL_DIR,
                1,
                owners.normal_uid,
                &append_lock,
                &mut dup_obs,
            )
            .expect_err("duplicate create_segment_file_at_fd_unix must fail with AlreadyExists");
            assert!(matches!(
                dup_err,
                StorageBackendError::Io {
                    kind: io::ErrorKind::AlreadyExists,
                    ..
                }
            ));
            drop(append_lock);

            // Empty contiguous successor seg 1 leaves the lane 100% recoverable via Case D.
            let rec3 = recover_lane(repo3.path(), CaptureQueueLane::Normal)
                .expect("lane with contiguous empty seg 1 must recover cleanly");
            assert_eq!(rec3.meta.active_seg, 0);
            assert!(!seg1_path3.exists());
        }
    }

    /// `RB-08m` AC1–AC8:
    /// - AC1: On valid `LTM2` with verified `[0, committed_len)` prefix and a
    ///   trailing truncated half-frame, Step C1 invokes `RB-08t`
    ///   `quarantine_and_truncate_segment` -> `publish_quarantine_durable` to
    ///   durably persist `[recovered_valid_end, actual_len)` into `quarantine/`.
    /// - AC2: Quarantined tail records exact diagnostic code `LBR-AGENT-050`.
    /// - AC3: Strictly AFTER `fsync_dir(quarantine_dir)`, `events-<S>.log` is
    ///   truncated in-place to exact `recovered_valid_end`.
    /// - AC4: Immediately after truncation, `events-<S>.log` executes `sync_data()`.
    /// - AC5: Strictly BEFORE creating any `S + 1` file (zero `S + 1` created
    ///   in Step C1), dual-replica `LTM2` commits `COMMITTED_LEN = recovered_valid_end`.
    /// - AC6: Same Step C1 dual-replica `LTM2` commit sets `PREFIX_CRC32C` equal
    ///   to `crc32c(events-<S>.log[0..recovered_valid_end])`.
    /// - AC7: All pre-crash committed prefix frames `[0, old_committed_len)`
    ///   decode 100% losslessly from segment `S`.
    /// - AC8: All recovered complete synced tail frames
    ///   `[old_committed_len, recovered_valid_end)` decode 100% losslessly from `S`.
    #[test]
    fn case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s() {
        // ------------------------------------------------------------------
        // Part 1 — `recover_lane` Step C1 with prefix frames + synced tail
        // frames (including >64 KiB streaming payload) + truncated half-frame
        // ------------------------------------------------------------------
        let repo = tempdir().expect("rb08m tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");

        let p1_payload = b"prefix-committed-frame-1".to_vec();
        let p2_payload = b"prefix-committed-frame-2-longer-body".to_vec();
        let out_p1 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08m-p1",
            &p1_payload,
        )
        .expect("append prefix frame 1");
        let out_p2 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08m-p2",
            &p2_payload,
        )
        .expect("append prefix frame 2");
        let old_committed_len = out_p2.meta.committed_len;
        let old_prefix_crc = out_p2.meta.prefix_crc32c;
        assert_eq!(
            old_committed_len,
            (out_p1.frame_len + out_p2.frame_len) as u64
        );

        // Append 2 complete synced uncommitted tail frames (T1 small, T2 >64 KiB
        // to exercise multi-chunk streaming CRC32C) + genuine truncated
        // `encode_frame` half-frame H (`TruncatedTail`) directly to seg 0.
        let t1_payload = b"synced-uncommitted-tail-frame-1".to_vec();
        let t2_payload = vec![b'T'; 80 * 1024];
        let t1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08m-t1", &t1_payload).unwrap();
        let t2_bytes = encode_frame(FRAME_FLAG_NONE, "rb08m-t2-80k", &t2_payload).unwrap();
        let full_h = encode_frame(FRAME_FLAG_NONE, "rb08m-half-frame", &vec![b'H'; 4096]).unwrap();
        let half_frame_tail = full_h[..full_h.len() / 2].to_vec();

        let seg0_path = init.paths.normal.join(segment_log_file_name(0));
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open seg0 for Case C injection");
            f.write_all(&t1_bytes).unwrap();
            f.write_all(&t2_bytes).unwrap();
            f.write_all(&half_frame_tail).unwrap();
            f.sync_data().unwrap();
        }
        let recovered_valid_end = old_committed_len + t1_bytes.len() as u64 + t2_bytes.len() as u64;
        let expected_recovered_crc =
            crc32c_append(crc32c_append(old_prefix_crc, &t1_bytes), &t2_bytes);
        let expected_q_id = quarantine_artifact_id(
            CaptureQueueLane::Normal,
            0,
            recovered_valid_end,
            &half_frame_tail,
        );
        let expected_q_bin = init.paths.quarantine.join(format!("{expected_q_id}.bin"));

        // Pause deterministically immediately after Step C1 commits dual `LTM2`
        // and strictly BEFORE Step C2 (`RB-08n`) creates `events-00000001.log`,
        // verifying ALL 8 `RB-08m` ACs directly on the Step C1 persisted state.
        let mut obs =
            SegmentObserver::with_crash(SegmentCrashPoint::AfterCaseCStepC1MetaBeforeStepC2);
        let c1_err = recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs)
            .expect_err("simulated crash right after Step C1 LTM2 must halt before Step C2");
        assert_eq!(c1_err.stable_code_str(), "LBR-IO-001");

        // AC1 + AC2: Durable quarantine artifact with exact bytes & `LBR-AGENT-050`.
        assert_eq!(
            fs::read(&expected_q_bin).expect("AC1: read quarantine .bin"),
            half_frame_tail
        );
        assert!(
            !init
                .paths
                .quarantine
                .join(format!("{expected_q_id}.tmp"))
                .exists()
        );
        assert_eq!(QUARANTINE_DIAGNOSTIC_CODE, "LBR-AGENT-050");

        // AC3 + AC4 + AC5 strict barrier ordering proof via SegmentObserver:
        //   QuarantineTmpSyncDataComplete
        //   < QuarantineRenameStart < QuarantineRenameComplete
        //   < QuarantineFsyncDirStart < QuarantineFsyncDirComplete
        //   < SegmentTruncateStart < SegmentTruncateComplete
        //   < SegmentSyncData(seg 0)
        //   < MetaTmpWriteAndSync(tail.meta) < MetaRename(tail.meta) < FsyncLaneDirAfterMeta(tail.meta)
        //   < MetaTmpWriteAndSync(tail.meta.bak) < MetaRename(tail.meta.bak) < FsyncLaneDirAfterMeta(tail.meta.bak)
        let pos_q_sync = obs
            .events
            .iter()
            .position(|e| {
                matches!(e, SegmentOpEvent::QuarantineTmpSyncDataComplete { id } if id == &expected_q_id)
            })
            .expect("quarantine tmp sync complete");
        let pos_q_rename_done = obs
            .events
            .iter()
            .position(|e| {
                matches!(e, SegmentOpEvent::QuarantineRenameComplete { id } if id == &expected_q_id)
            })
            .expect("quarantine rename complete");
        let pos_q_fsync_done = obs
            .events
            .iter()
            .position(|e| {
                matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &expected_q_id)
            })
            .expect("quarantine fsync_dir complete");
        let pos_trunc_start = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                    to_len: recovered_valid_end,
                }
            })
            .expect("segment truncate start");
        let pos_trunc_done = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateComplete {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                    to_len: recovered_valid_end,
                }
            })
            .expect("segment truncate complete");
        let pos_seg_sync = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentSyncData {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                }
            })
            .expect("AC4: post-truncate seg.sync_data()");
        let pos_meta_primary_tmp = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::MetaTmpWriteAndSync {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    tmp_name: QUEUE_TAIL_META_TMP_FILE,
                    target_name: QUEUE_TAIL_META_FILE,
                }
            })
            .expect("AC5: primary tail.meta commit");
        let pos_meta_bak_fsync = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("AC5: backup tail.meta.bak fsync_dir");

        assert!(pos_q_sync < pos_q_rename_done);
        assert!(pos_q_rename_done < pos_q_fsync_done);
        assert!(
            pos_q_fsync_done < pos_trunc_start,
            "AC3 violated: fsync_dir(quarantine_dir) must complete before truncate"
        );
        assert!(pos_trunc_start < pos_trunc_done);
        assert!(
            pos_trunc_done < pos_seg_sync,
            "AC4 violated: truncate must complete before seg.sync_data()"
        );
        assert!(
            pos_seg_sync < pos_meta_primary_tmp,
            "AC5 violated: seg.sync_data() must complete before LTM2 commit"
        );
        assert!(pos_meta_primary_tmp < pos_meta_bak_fsync);
        // Step C1 commits dual `LTM2` strictly BEFORE any `S + 1` file is created.
        assert!(
            !obs.events
                .iter()
                .any(|e| matches!(e, SegmentOpEvent::CreateSegment { .. })),
            "AC5 violated: Step C1 must commit LTM2 before any S+1 segment creation"
        );
        assert!(!init.paths.normal.join(segment_log_file_name(1)).exists());

        // AC3 on-disk truncated length.
        let seg0_after = fs::read(&seg0_path).expect("read truncated seg0");
        assert_eq!(
            seg0_after.len() as u64,
            recovered_valid_end,
            "AC3 violated: seg0 length must equal exact recovered_valid_end"
        );

        // AC5 + AC6 on-disk dual `LTM2` replicas right after Step C1.
        let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
        let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
        assert_eq!(raw_meta, raw_bak);
        let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
        assert_eq!(disk_meta.active_seg, 0);
        assert!(disk_meta.sealed.is_empty());
        assert_eq!(
            disk_meta.committed_len, recovered_valid_end,
            "AC5 violated: LTM2.COMMITTED_LEN must equal recovered_valid_end"
        );
        assert_eq!(
            disk_meta.prefix_crc32c, expected_recovered_crc,
            "AC6 violated: LTM2.PREFIX_CRC32C must match recovered_crc32c"
        );
        assert_eq!(
            crc32c_checksum(&seg0_after),
            disk_meta.prefix_crc32c,
            "AC6 violated: on-disk seg0 CRC32C must match LTM2.PREFIX_CRC32C"
        );
        // Clean reopen after Step C1 finds Case A clean on seg 0 with zero extra work.
        let clean_reopen = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("clean reopen after Step C1 must succeed");
        assert_eq!(clean_reopen.meta, disk_meta);

        // AC7: All prefix old frames `[0, old_committed_len)` decode 100% losslessly.
        let prefix_decoded = decode_all_segment_bytes(&seg0_after[..old_committed_len as usize]);
        assert_eq!(prefix_decoded.len(), 2);
        assert_eq!(prefix_decoded[0].dedup_key, "rb08m-p1");
        assert_eq!(prefix_decoded[0].payload, p1_payload);
        assert_eq!(prefix_decoded[1].dedup_key, "rb08m-p2");
        assert_eq!(prefix_decoded[1].payload, p2_payload);

        // AC8: All recovered complete synced tail frames `[old_committed_len, recovered_valid_end)`
        // decode 100% losslessly.
        let tail_decoded = decode_all_segment_bytes(&seg0_after[old_committed_len as usize..]);
        assert_eq!(tail_decoded.len(), 2);
        assert_eq!(tail_decoded[0].dedup_key, "rb08m-t1");
        assert_eq!(tail_decoded[0].payload, t1_payload);
        assert_eq!(tail_decoded[1].dedup_key, "rb08m-t2-80k");
        assert_eq!(tail_decoded[1].payload, t2_payload);

        // ------------------------------------------------------------------
        // Part 2a — Producer path on zero-synced-tail Case C damaged tail
        // (`recovered_valid_end == old_committed_len < actual_len`): verifies
        // Step C1 dual `LTM2` strictly precedes Step C2 `CreateSegment(1)`,
        // and the incoming frame lands in `seg 1` at offset `0`.
        // ------------------------------------------------------------------
        let corrupt_hdr_tail = b"BAD!\xff\xff\xff\xfftruncated-corrupt-header".to_vec();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .unwrap();
            f.write_all(&corrupt_hdr_tail).unwrap();
            f.sync_data().unwrap();
        }
        let n1_payload = b"new-frame-after-step-c1".to_vec();
        let n1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08m-n1", &n1_payload).unwrap();
        let mut app_obs = SegmentObserver::new();
        let app_out = append_validated_frame_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            &n1_bytes,
            &mut app_obs,
        )
        .expect("append_frame after zero-synced-tail Step C1+C2 must succeed");
        assert_eq!(app_out.seg_id, 1);
        assert_eq!(app_out.frame_offset, 0);
        assert_eq!(app_out.recovered_frames, 0);
        assert_eq!(app_out.meta.sealed.len(), 1);
        assert_eq!(app_out.meta.sealed[0].seg_id, 0);
        assert_eq!(app_out.meta.sealed[0].sealed_len, recovered_valid_end);
        let q2_id = quarantine_artifact_id(
            CaptureQueueLane::Normal,
            0,
            recovered_valid_end,
            &corrupt_hdr_tail,
        );
        assert_eq!(
            fs::read(init.paths.quarantine.join(format!("{q2_id}.bin"))).unwrap(),
            corrupt_hdr_tail
        );
        // Verify Step C1 completed dual `LTM2` strictly BEFORE Step C2
        // `CreateSegment(1)` AND before incoming frame `write_all` into seg 1.
        let pos_c1_bak = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("Step C1 LTM2 backup commit");
        let pos_c2_create = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::CreateSegment {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("Step C2 create seg 1");
        let pos_new_write = app_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentWriteAll {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                    offset: 0,
                    len: n1_bytes.len(),
                }
            })
            .expect("incoming frame write_all into seg 1");
        assert!(pos_c1_bak < pos_c2_create && pos_c2_create < pos_new_write);

        // ------------------------------------------------------------------
        // Part 2b — Public `append_frame` producer path with NON-ZERO
        // complete synced uncommitted tail (`old_committed_len <
        // recovered_valid_end < actual_len`) followed by a damaged half-frame
        // on active seg 1 (sealing seg 1 and appending new frame to seg 2).
        // ------------------------------------------------------------------
        let seg1_path = init.paths.normal.join(segment_log_file_name(1));
        let pre_pub_committed_len = app_out.meta.committed_len;
        let u1_payload = b"uncommitted-complete-tail-u1".to_vec();
        let u2_payload = vec![0x6bu8; 4096];
        let u1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08m-u1", &u1_payload).unwrap();
        let u2_bytes = encode_frame(FRAME_FLAG_NONE, "rb08m-u2", &u2_payload).unwrap();
        let pub_half_src = encode_frame(
            FRAME_FLAG_NONE,
            "rb08m-pub-half",
            b"half-frame-before-public-append-frame",
        )
        .unwrap();
        let pub_half_tail = pub_half_src[..pub_half_src.len() - 9].to_vec();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg1_path)
                .unwrap();
            f.write_all(&u1_bytes).unwrap();
            f.write_all(&u2_bytes).unwrap();
            f.write_all(&pub_half_tail).unwrap();
            f.sync_data().unwrap();
        }
        let expected_pub_recovered_end =
            pre_pub_committed_len + (u1_bytes.len() + u2_bytes.len()) as u64;
        let pub_payload = b"public-append-frame-after-nonzero-synced-tail-c1".to_vec();
        let pub_encoded = encode_frame(FRAME_FLAG_NONE, "rb08m-pub-n2", &pub_payload).unwrap();
        let pub_out = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08m-pub-n2",
            &pub_payload,
        )
        .expect("public append_frame after non-zero synced-tail Step C1+C2 must succeed");
        assert_eq!(pub_out.seg_id, 2);
        assert_eq!(pub_out.frame_offset, 0);
        assert_eq!(pub_out.frame_len, pub_encoded.len());
        assert_eq!(pub_out.recovered_frames, 2);
        assert_eq!(pub_out.meta.active_seg, 2);
        assert_eq!(pub_out.meta.sealed.len(), 2);
        assert_eq!(pub_out.meta.sealed[1].seg_id, 1);
        assert_eq!(
            pub_out.meta.sealed[1].sealed_len,
            expected_pub_recovered_end
        );
        assert_eq!(pub_out.meta.committed_len, pub_encoded.len() as u64);
        let q3_id = quarantine_artifact_id(
            CaptureQueueLane::Normal,
            1,
            expected_pub_recovered_end,
            &pub_half_tail,
        );
        assert!(
            !init.paths.quarantine.join(format!("{q3_id}.tmp")).exists(),
            "quarantine temp file must be atomically renamed to .bin"
        );
        assert_eq!(
            fs::read(init.paths.quarantine.join(format!("{q3_id}.bin"))).unwrap(),
            pub_half_tail
        );
        let seg1_after_pub = fs::read(&seg1_path).unwrap();
        assert_eq!(seg1_after_pub.len() as u64, expected_pub_recovered_end);
        assert_eq!(
            pub_out.meta.sealed[1].sealed_crc32c,
            crc32c::crc32c(&seg1_after_pub)
        );
        let seg2_path = init.paths.normal.join(segment_log_file_name(2));
        let seg2_after_pub = fs::read(&seg2_path).unwrap();
        assert_eq!(seg2_after_pub.len() as u64, pub_out.meta.committed_len);
        assert_eq!(pub_out.meta.prefix_crc32c, crc32c::crc32c(&seg2_after_pub));
        let meta_after_pub = read_converged_lane_tail_meta(&init.paths.normal)
            .expect("converged LTM2 after public append_frame");
        assert_eq!(meta_after_pub, pub_out.meta);

        let mut all_frames = decode_all_segment_bytes(&fs::read(&seg0_path).unwrap());
        all_frames.extend(decode_all_segment_bytes(&seg1_after_pub));
        all_frames.extend(decode_all_segment_bytes(&seg2_after_pub));
        assert_eq!(all_frames.len(), 8);
        assert_eq!(
            all_frames
                .iter()
                .map(|f| f.dedup_key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "rb08m-p1",
                "rb08m-p2",
                "rb08m-t1",
                "rb08m-t2-80k",
                "rb08m-n1",
                "rb08m-u1",
                "rb08m-u2",
                "rb08m-pub-n2",
            ]
        );
        assert_eq!(all_frames[5].payload, u1_payload);
        assert_eq!(all_frames[6].payload, u2_payload);
        assert_eq!(all_frames[7].payload, pub_payload);

        // ------------------------------------------------------------------
        // Part 3 — Prior guard: corrupt committed prefix `[0, committed_len)`
        // + damaged uncommitted tail on active seg 2 MUST still fail closed
        // with ZERO disk mutation (never quarantining, truncating, or rotating!)
        // ------------------------------------------------------------------
        let mut bad_seg2 = fs::read(&seg2_path).unwrap();
        bad_seg2[0] ^= 0x01; // corrupt committed prefix byte 0
        bad_seg2.extend_from_slice(&half_frame_tail);
        write_bytes_0600(&seg2_path, &bad_seg2);
        let snap_corrupt = snapshot_tree(&init.paths.root);
        let prefix_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect_err("corrupt committed prefix + damaged tail must fail closed");
        assert!(matches!(
            prefix_err,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));
        assert_eq!(prefix_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(snapshot_tree(&init.paths.root), snap_corrupt);
    }

    /// `RB-08n` AC1–AC7: Base Case C **Step C2** (`seal_recovered_segment_to_next`)
    /// after Step C1 truncates a non-empty segment: reuses shared
    /// `create_segment_file` (`0600` + `fsync_dir`) to create `events-<S+1:08>.log`,
    /// atomically commits dual-replica `LTM2` sealing `(S, recovered_valid_end,
    /// recovered_crc32c)` into `LTM2.sealed[]`, advancing `ACTIVE_SEG = S + 1`,
    /// resetting `COMMITTED_LEN = 0` and `PREFIX_CRC32C = 0`, and verifies
    /// subsequent appends to `S + 1` succeed and decode 100% losslessly (plus
    /// single-call `append_frame` rotation to `S + 2` and clean synced tail
    /// staying on `ACTIVE_SEG` without Step C2).
    #[test]
    fn case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1() {
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");

        // Seed segment 0 with 2 committed frames + 1 complete synced tail frame
        // + 1 genuine truncated half-frame tail.
        let p1_payload = b"rb08n-prefix-frame-1".to_vec();
        let p2_payload = b"rb08n-prefix-frame-2".to_vec();
        let _out_p1 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08n-p1",
            &p1_payload,
        )
        .expect("append prefix frame 1");
        let out_p2 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08n-p2",
            &p2_payload,
        )
        .expect("append prefix frame 2");
        let old_committed_len = out_p2.meta.committed_len;
        let old_prefix_crc = out_p2.meta.prefix_crc32c;

        let t1_payload = vec![b'S'; 70 * 1024];
        let t1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08n-t1-70k", &t1_payload).unwrap();
        let half_src =
            encode_frame(FRAME_FLAG_NONE, "rb08n-half-s0", b"truncated-tail-on-seg0").unwrap();
        let half_tail = half_src[..half_src.len() / 2].to_vec();

        let seg0_path = init.paths.normal.join(segment_log_file_name(0));
        let seg1_path = init.paths.normal.join(segment_log_file_name(1));
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .expect("open seg0");
            f.write_all(&t1_bytes).unwrap();
            f.write_all(&half_tail).unwrap();
            f.sync_data().unwrap();
        }
        let recovered_valid_end = old_committed_len + t1_bytes.len() as u64;
        let expected_recovered_crc = crc32c_append(old_prefix_crc, &t1_bytes);
        let expected_q_id =
            quarantine_artifact_id(CaptureQueueLane::Normal, 0, recovered_valid_end, &half_tail);

        // Execute full Case C recovery (Step C1 -> Step C2).
        let mut obs = SegmentObserver::new();
        let rec = recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs)
            .expect("Case C Step C1 + Step C2 recover_lane must succeed");

        // AC1: Step C2 reused shared `create_segment_file` (`0600` + `fsync_dir`)
        // to create `events-00000001.log` strictly AFTER Step C1 dual `LTM2`
        // commit and strictly BEFORE Step C2 dual `LTM2` commit.
        assert!(
            seg1_path.exists(),
            "AC1 violated: events-00000001.log must exist after Step C2"
        );
        assert_eq!(
            fs::read(&seg1_path).expect("read empty seg1"),
            Vec::<u8>::new()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&seg1_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "AC1 violated: Step C2 segment S+1 must inherit 0600 mode from create_segment_file"
            );
        }

        let pos_q_fsync_done = obs
            .events
            .iter()
            .position(|e| {
                matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &expected_q_id)
            })
            .expect("quarantine fsync_dir complete");
        let pos_seg0_sync = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentSyncData {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                }
            })
            .expect("Step C1 seg0 sync_data");
        let meta_bak_positions: Vec<usize> = obs
            .events
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                (*e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                })
                .then_some(idx)
            })
            .collect();
        assert_eq!(
            meta_bak_positions.len(),
            2,
            "Step C1 and Step C2 must each commit dual LTM2 replicas"
        );
        let pos_c1_bak_fsync = meta_bak_positions[0];
        let pos_c2_bak_fsync = meta_bak_positions[1];
        let pos_c2_create = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::CreateSegment {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC1: Step C2 CreateSegment(1)");
        let pos_c2_create_fsync = obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterCreateSegment {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("AC1: Step C2 FsyncLaneDirAfterCreateSegment(1)");
        let pos_unlock = obs
            .events
            .iter()
            .position(|e| *e == SegmentOpEvent::ReleaseAppendLock)
            .expect("release append.lock");

        assert!(pos_q_fsync_done < pos_seg0_sync);
        assert!(pos_seg0_sync < pos_c1_bak_fsync);
        assert!(
            pos_c1_bak_fsync < pos_c2_create,
            "Step C1 dual LTM2 commit must complete strictly before Step C2 CreateSegment(S+1)"
        );
        assert!(pos_c2_create < pos_c2_create_fsync);
        assert!(
            pos_c2_create_fsync < pos_c2_bak_fsync,
            "Step C2 fsync_dir(lane_dir) after create_segment_file must precede Step C2 LTM2 commit"
        );
        assert!(pos_c2_bak_fsync < pos_unlock);

        // AC2 + AC3 + AC4 + AC5: Dual-replica on-disk `LTM2` seals old segment 0,
        // advances `ACTIVE_SEG = 1`, and resets `COMMITTED_LEN = 0` & `PREFIX_CRC32C = 0`.
        let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
        let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
        assert_eq!(raw_meta, raw_bak);
        let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
        assert_eq!(disk_meta, rec.meta);
        assert_eq!(
            disk_meta.sealed,
            vec![SealedSegmentMeta {
                seg_id: 0,
                sealed_len: recovered_valid_end,
                sealed_crc32c: expected_recovered_crc,
            }],
            "AC2 violated: Step C2 must seal (0, recovered_valid_end, recovered_crc32c) into LTM2.sealed[]"
        );
        assert_eq!(
            disk_meta.active_seg, 1,
            "AC3 violated: Step C2 must advance LTM2.ACTIVE_SEG to S + 1"
        );
        assert_eq!(
            disk_meta.committed_len, 0,
            "AC4 violated: Step C2 must reset new segment LTM2.COMMITTED_LEN to 0"
        );
        assert_eq!(
            disk_meta.prefix_crc32c, 0,
            "AC5 violated: Step C2 must reset new segment LTM2.PREFIX_CRC32C to 0"
        );
        assert_eq!(rec.recovered_frames, 1);
        assert_eq!(rec.recovered_bytes, t1_bytes.len() as u64);
        assert_eq!(rec.quarantined_artifacts.len(), 1);
        assert_eq!(rec.quarantined_artifacts[0].id, expected_q_id);
        assert_eq!(
            rec.quarantined_artifacts[0].diagnostic_code,
            QUARANTINE_DIAGNOSTIC_CODE
        );

        // Sealed seg 0 retains exact truncated bytes and decodes all 3 frames losslessly.
        let seg0_bytes = fs::read(&seg0_path).unwrap();
        assert_eq!(seg0_bytes.len() as u64, recovered_valid_end);
        assert_eq!(crc32c_checksum(&seg0_bytes), expected_recovered_crc);
        let seg0_decoded = decode_all_segment_bytes(&seg0_bytes);
        assert_eq!(
            seg0_decoded
                .iter()
                .map(|f| f.dedup_key.as_str())
                .collect::<Vec<_>>(),
            vec!["rb08n-p1", "rb08n-p2", "rb08n-t1-70k"]
        );
        assert_eq!(seg0_decoded[0].payload, p1_payload);
        assert_eq!(seg0_decoded[1].payload, p2_payload);
        assert_eq!(seg0_decoded[2].payload, t1_payload);

        // AC6: Subsequent public `append_frame` calls onto new active segment S + 1
        // return `Ok(...)` landing at seg 1 offset 0 and advancing seg 1 cleanly.
        let s1_f1_payload = b"rb08n-new-frame-in-s1-first".to_vec();
        let s1_f2_payload = vec![0x4au8; 72 * 1024];
        let out_s1_f1 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08n-s1-f1",
            &s1_f1_payload,
        )
        .expect("AC6: append frame 1 to new active segment S+1 must return Ok");
        assert_eq!(out_s1_f1.seg_id, 1);
        assert_eq!(out_s1_f1.frame_offset, 0);
        assert_eq!(out_s1_f1.meta.active_seg, 1);
        assert_eq!(out_s1_f1.meta.committed_len, out_s1_f1.frame_len as u64);
        assert_eq!(out_s1_f1.meta.sealed, disk_meta.sealed);

        let out_s1_f2 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08n-s1-f2-72k",
            &s1_f2_payload,
        )
        .expect("AC6: append frame 2 to new active segment S+1 must return Ok");
        assert_eq!(out_s1_f2.seg_id, 1);
        assert_eq!(out_s1_f2.frame_offset, out_s1_f1.frame_len as u64);
        assert_eq!(
            out_s1_f2.meta.committed_len,
            (out_s1_f1.frame_len + out_s1_f2.frame_len) as u64
        );
        assert_eq!(out_s1_f2.meta.sealed, disk_meta.sealed);

        // AC7: Newly appended frames in `events-00000001.log` decode 100% losslessly.
        let seg1_bytes = fs::read(&seg1_path).unwrap();
        assert_eq!(seg1_bytes.len() as u64, out_s1_f2.meta.committed_len);
        assert_eq!(crc32c_checksum(&seg1_bytes), out_s1_f2.meta.prefix_crc32c);
        let seg1_decoded = decode_all_segment_bytes(&seg1_bytes);
        assert_eq!(seg1_decoded.len(), 2);
        assert_eq!(seg1_decoded[0].dedup_key, "rb08n-s1-f1");
        assert_eq!(seg1_decoded[0].payload, s1_f1_payload);
        assert_eq!(seg1_decoded[1].dedup_key, "rb08n-s1-f2-72k");
        assert_eq!(seg1_decoded[1].payload, s1_f2_payload);

        // Part 2 — Single-call `append_frame` on damaged seg 1 performs Step C1 +
        // Step C2 (sealing seg 1 into `sealed[1]`, creating `events-00000002.log`)
        // and writes the incoming frame directly into seg 2 at offset 0.
        let half_s1_src = encode_frame(FRAME_FLAG_NONE, "rb08n-half-s1", b"seg1-half").unwrap();
        let half_s1_tail = half_s1_src[..half_s1_src.len() - 5].to_vec();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg1_path)
                .unwrap();
            f.write_all(&half_s1_tail).unwrap();
            f.sync_data().unwrap();
        }
        let s2_payload = b"rb08n-single-call-append-into-s2".to_vec();
        let out_s2 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08n-s2-f1",
            &s2_payload,
        )
        .expect("single-call append_frame triggering Step C1+C2 must succeed");
        assert_eq!(out_s2.seg_id, 2);
        assert_eq!(out_s2.frame_offset, 0);
        assert_eq!(out_s2.meta.active_seg, 2);
        assert_eq!(out_s2.meta.sealed.len(), 2);
        assert_eq!(out_s2.meta.sealed[0], disk_meta.sealed[0]);
        assert_eq!(
            out_s2.meta.sealed[1],
            SealedSegmentMeta {
                seg_id: 1,
                sealed_len: out_s1_f2.meta.committed_len,
                sealed_crc32c: out_s1_f2.meta.prefix_crc32c,
            }
        );
        let seg2_path = init.paths.normal.join(segment_log_file_name(2));
        let seg2_decoded = decode_all_segment_bytes(&fs::read(&seg2_path).unwrap());
        assert_eq!(seg2_decoded.len(), 1);
        assert_eq!(seg2_decoded[0].dedup_key, "rb08n-s2-f1");
        assert_eq!(seg2_decoded[0].payload, s2_payload);

        // Part 3 — Pure synced uncommitted tail (`!has_damaged_tail`) MUST stay
        // on `ACTIVE_SEG == 2` without invoking Step C2 or creating seg 3.
        let clean_tail_payload = b"rb08n-pure-synced-tail-no-step-c2".to_vec();
        let clean_tail_bytes =
            encode_frame(FRAME_FLAG_NONE, "rb08n-s2-clean-tail", &clean_tail_payload).unwrap();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg2_path)
                .unwrap();
            f.write_all(&clean_tail_bytes).unwrap();
            f.sync_data().unwrap();
        }
        let rec_clean = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("pure synced tail recovery must succeed");
        assert_eq!(rec_clean.meta.active_seg, 2);
        assert_eq!(rec_clean.meta.sealed.len(), 2);
        assert!(rec_clean.quarantined_artifacts.is_empty());
        assert_eq!(rec_clean.recovered_frames, 1);
        assert!(!init.paths.normal.join(segment_log_file_name(3)).exists());
        let seg2_after_clean = decode_all_segment_bytes(&fs::read(&seg2_path).unwrap());
        assert_eq!(seg2_after_clean.len(), 2);
        assert_eq!(seg2_after_clean[1].dedup_key, "rb08n-s2-clean-tail");
        assert_eq!(seg2_after_clean[1].payload, clean_tail_payload);
    }

    /// `RB-08i` AC1–AC8: Deterministic fault-injection verification across all
    /// Case C Step C1 / Step C2 crash boundaries and caller-side ordering:
    /// - AC8: crash inside `case_c_step_c1` strictly BEFORE quarantine `rename`
    ///   leaves `events-<S>.log` byte-for-byte identical (zero premature truncation),
    ///   preserves sibling lane `.tmp`, and recovers cleanly on restart (both
    ///   `recover_lane` and `append_frame` paths).
    /// - AC1 + AC7: Boundary 1 (crash strictly AFTER `fsync_dir(quarantine_dir)`
    ///   and BEFORE `truncate(S)`): restart idempotently re-runs Case C with zero
    ///   duplicate quarantine files, asserts strict observer ordering
    ///   `QuarantineFsyncDirComplete < SegmentTruncateStart` on BOTH `recover_lane`
    ///   and `append_frame`, and retains 100% of complete synced frames in `S`.
    /// - AC2 + AC3: Boundary 2 (crash strictly AFTER `truncate(S)+sync_data()` and
    ///   BEFORE Step C1 `LTM2`): restart detects clean segment `S` up to
    ///   `recovered_valid_end` (zero new quarantine), advances `LTM2.COMMITTED_LEN`
    ///   to `recovered_valid_end`, and keeps `LTM2.ACTIVE_SEG == S` (tested for
    ///   both non-zero synced tail and zero synced tail).
    /// - AC4 + AC5 + AC6: Boundary 3 (crash strictly AFTER Step C1 + creating empty
    ///   `S+1` and BEFORE Step C2 `LTM2`): Case D unlinks empty `events-<S+1>.log`,
    ///   keeps `LTM2.ACTIVE_SEG == S`, and preserves authenticated
    ///   `LTM2.COMMITTED_LEN == recovered_valid_end` (tested on both `recover_lane`
    ///   and `append_frame` restart paths).
    #[test]
    fn case_c_recovery_crash_boundaries_1_2_3_converge() {
        /// Helper to initialize a fresh repo with 2 committed prefix frames +
        /// 2 complete synced uncommitted tail frames (one >64 KiB) + 1 genuine
        /// truncated half-frame on `Normal` seg 0.
        struct CaseCFixture {
            repo: tempfile::TempDir,
            init: crate::storage::queue::CaptureQueueInitReport,
            seg0_path: PathBuf,
            seg1_path: PathBuf,
            old_committed_len: u64,
            recovered_valid_end: u64,
            expected_recovered_crc: u32,
            expected_q_id: String,
            expected_q_bin: PathBuf,
            expected_q_tmp: PathBuf,
            pre_crash_seg0_bytes: Vec<u8>,
            half_frame_tail: Vec<u8>,
            p1_payload: Vec<u8>,
            p2_payload: Vec<u8>,
            t1_payload: Vec<u8>,
            t2_payload: Vec<u8>,
        }

        fn make_case_c_fixture(tag: &str) -> CaseCFixture {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let p1_payload = format!("{tag}-prefix-1").into_bytes();
            let p2_payload = format!("{tag}-prefix-2-body").into_bytes();
            let _out_p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &format!("{tag}-p1"),
                &p1_payload,
            )
            .expect("append prefix 1");
            let out_p2 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                &format!("{tag}-p2"),
                &p2_payload,
            )
            .expect("append prefix 2");
            let old_committed_len = out_p2.meta.committed_len;
            let old_prefix_crc = out_p2.meta.prefix_crc32c;

            let t1_payload = format!("{tag}-synced-tail-1").into_bytes();
            let t2_payload = vec![0x55u8; 72 * 1024];
            let t1_bytes = encode_frame(FRAME_FLAG_NONE, &format!("{tag}-t1"), &t1_payload)
                .expect("encode t1");
            let t2_bytes = encode_frame(FRAME_FLAG_NONE, &format!("{tag}-t2-72k"), &t2_payload)
                .expect("encode t2");
            let half_src =
                encode_frame(FRAME_FLAG_NONE, &format!("{tag}-half"), &vec![0x7au8; 2048])
                    .expect("encode half source");
            let half_frame_tail = half_src[..half_src.len() / 2].to_vec();

            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .expect("open seg0");
                f.write_all(&t1_bytes).unwrap();
                f.write_all(&t2_bytes).unwrap();
                f.write_all(&half_frame_tail).unwrap();
                f.sync_data().unwrap();
            }
            let pre_crash_seg0_bytes = fs::read(&seg0_path).expect("read pre-crash seg0");
            let recovered_valid_end =
                old_committed_len + t1_bytes.len() as u64 + t2_bytes.len() as u64;
            let expected_recovered_crc =
                crc32c_append(crc32c_append(old_prefix_crc, &t1_bytes), &t2_bytes);
            let expected_q_id = quarantine_artifact_id(
                CaptureQueueLane::Normal,
                0,
                recovered_valid_end,
                &half_frame_tail,
            );
            let expected_q_bin = init.paths.quarantine.join(format!("{expected_q_id}.bin"));
            let expected_q_tmp = init.paths.quarantine.join(format!("{expected_q_id}.tmp"));

            CaseCFixture {
                repo,
                init,
                seg0_path,
                seg1_path,
                old_committed_len,
                recovered_valid_end,
                expected_recovered_crc,
                expected_q_id,
                expected_q_bin,
                expected_q_tmp,
                pre_crash_seg0_bytes,
                half_frame_tail,
                p1_payload,
                p2_payload,
                t1_payload,
                t2_payload,
            }
        }

        fn assert_seg0_all_4_frames_intact(fx: &CaseCFixture, tag: &str) {
            let seg0_bytes = fs::read(&fx.seg0_path).expect("read recovered seg0");
            assert_eq!(seg0_bytes.len() as u64, fx.recovered_valid_end);
            assert_eq!(crc32c_checksum(&seg0_bytes), fx.expected_recovered_crc);
            let decoded = decode_all_segment_bytes(&seg0_bytes);
            assert_eq!(decoded.len(), 4);
            assert_eq!(decoded[0].dedup_key, format!("{tag}-p1"));
            assert_eq!(decoded[0].payload, fx.p1_payload);
            assert_eq!(decoded[1].dedup_key, format!("{tag}-p2"));
            assert_eq!(decoded[1].payload, fx.p2_payload);
            assert_eq!(decoded[2].dedup_key, format!("{tag}-t1"));
            assert_eq!(decoded[2].payload, fx.t1_payload);
            assert_eq!(decoded[3].dedup_key, format!("{tag}-t2-72k"));
            assert_eq!(decoded[3].payload, fx.t2_payload);
        }

        // =====================================================================
        // AC8: Crash strictly BEFORE `rename(.tmp -> .bin)` inside real
        // `case_c_step_c1` MUST leave `events-<S>.log` byte-for-byte identical
        // (zero premature truncation) on BOTH `recover_lane` and `append_frame`.
        // =====================================================================
        {
            let fx = make_case_c_fixture("rb08i-ac8");
            // Plant a sibling lane's `.tmp` in `quarantine/` to verify isolation.
            let sibling_q_tmp = fx
                .init
                .paths
                .quarantine
                .join("priority-seg0-off0-deadbeef.tmp");
            write_bytes_0600(&sibling_q_tmp, b"priority-in-flight-tmp");

            let meta_before = fs::read(tail_meta_path(&fx.init.paths.normal)).unwrap();
            let bak_before = fs::read(tail_meta_bak_path(&fx.init.paths.normal)).unwrap();

            // 1) Crash via `recover_lane`.
            let mut obs_before_rename =
                SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
            let err = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_before_rename,
            )
            .expect_err("AC8: crash before quarantine rename must fail");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&fx.seg0_path).unwrap(),
                fx.pre_crash_seg0_bytes,
                "AC8 violated: events-00000000.log must be byte-for-byte identical before rename"
            );
            assert!(
                fx.expected_q_tmp.exists(),
                "AC8: quarantine .tmp preserved on simulated SIGKILL before rename"
            );
            assert!(
                !fx.expected_q_bin.exists(),
                "AC8: quarantine .bin must not exist before rename"
            );
            assert_eq!(
                fs::read(tail_meta_path(&fx.init.paths.normal)).unwrap(),
                meta_before
            );
            assert_eq!(
                fs::read(tail_meta_bak_path(&fx.init.paths.normal)).unwrap(),
                bak_before
            );
            assert!(
                !obs_before_rename
                    .events
                    .iter()
                    .any(|e| matches!(e, SegmentOpEvent::SegmentTruncateStart { .. })),
                "AC8 violated: truncate must never start when crash happens before quarantine rename"
            );

            // 2) Crash via `append_frame` producer path on the same pre-rename state:
            //    stale Normal `.tmp` is cleaned, recreated, synced, crashes again
            //    before rename — `events-00000000.log` STILL byte-for-byte untouched.
            let probe_frame = encode_frame(FRAME_FLAG_NONE, "rb08i-ac8-probe", b"probe").unwrap();
            let mut app_obs_before_rename =
                SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
            let app_err = append_validated_frame_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &probe_frame,
                &mut app_obs_before_rename,
            )
            .expect_err("AC8: append_frame crash before quarantine rename must fail");
            assert_eq!(app_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&fx.seg0_path).unwrap(),
                fx.pre_crash_seg0_bytes,
                "AC8 violated: producer path must leave events-00000000.log untouched before rename"
            );
            assert!(!fx.expected_q_bin.exists());
            assert!(
                sibling_q_tmp.exists(),
                "sibling lane quarantine .tmp must remain untouched"
            );

            // 3) Clean restart recovers idempotently, cleans Normal `.tmp`, keeps
            //    sibling `.tmp`, and preserves all 4 seg 0 frames.
            let rec = recover_lane(fx.repo.path(), CaptureQueueLane::Normal)
                .expect("AC8: clean restart after pre-rename crash must converge");
            assert_eq!(rec.cleaned_quarantine_tmp_files, 1);
            assert!(!fx.expected_q_tmp.exists());
            assert_eq!(fs::read(&fx.expected_q_bin).unwrap(), fx.half_frame_tail);
            assert!(sibling_q_tmp.exists());
            assert_seg0_all_4_frames_intact(&fx, "rb08i-ac8");
        }

        // =====================================================================
        // AC1 + AC7: Boundary 1 — crash strictly AFTER `fsync_dir(quarantine_dir)`
        // and BEFORE `truncate(S)`. Restart idempotently re-runs Case C with zero
        // duplicate quarantine artifacts, proves strict observer ordering
        // `QuarantineFsyncDirComplete < SegmentTruncateStart` on BOTH `recover_lane`
        // and `append_frame`, and retains 100% of complete synced frames in `S`.
        // =====================================================================
        {
            let fx = make_case_c_fixture("rb08i-b1");
            let mut obs_crash_b1 = SegmentObserver::with_crash(
                SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
            );
            let b1_err = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_crash_b1,
            )
            .expect_err("Boundary 1: crash after quarantine fsync_dir before truncate must fail");
            assert_eq!(b1_err.stable_code_str(), "LBR-IO-001");
            // Post-crash verification: `.bin` already persisted, seg 0 still untrucated.
            assert_eq!(fs::read(&fx.expected_q_bin).unwrap(), fx.half_frame_tail);
            assert!(!fx.expected_q_tmp.exists());
            assert_eq!(fs::read(&fx.seg0_path).unwrap(), fx.pre_crash_seg0_bytes);
            assert_eq!(
                read_converged_lane_tail_meta(&fx.init.paths.normal)
                    .unwrap()
                    .committed_len,
                fx.old_committed_len
            );

            // Idempotent restart via `recover_lane` with observer (AC1 + AC7).
            let mut obs_replay_b1 = SegmentObserver::new();
            let rec_b1 = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_replay_b1,
            )
            .expect("AC1: idempotent restart after Boundary 1 must converge");
            assert_eq!(rec_b1.recovered_frames, 2);
            assert_eq!(
                rec_b1.recovered_bytes,
                fx.recovered_valid_end - fx.old_committed_len
            );
            assert_eq!(rec_b1.quarantined_artifacts.len(), 1);
            assert_eq!(rec_b1.quarantined_artifacts[0].id, fx.expected_q_id);
            assert_eq!(
                rec_b1.quarantined_artifacts[0].diagnostic_code,
                QUARANTINE_DIAGNOSTIC_CODE
            );
            // Zero duplicate quarantine files in `quarantine/`.
            let q_entries: Vec<_> = fs::read_dir(&fx.init.paths.quarantine)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(
                q_entries,
                vec![std::ffi::OsString::from(format!(
                    "{}.bin",
                    fx.expected_q_id
                ))],
                "AC1 violated: Boundary 1 idempotent replay must produce zero duplicate quarantine files"
            );
            assert_eq!(fs::read(&fx.expected_q_bin).unwrap(), fx.half_frame_tail);

            // AC7: Assert caller-side observer ordering `fsync_dir(quarantine_dir) < truncate(S)`.
            let pos_q_fsync_done = obs_replay_b1
                .events
                .iter()
                .position(|e| {
                    matches!(
                        e,
                        SegmentOpEvent::QuarantineFsyncDirComplete { id }
                            if id == &fx.expected_q_id
                    )
                })
                .expect("AC7: QuarantineFsyncDirComplete in recover_lane");
            let pos_trunc_start = obs_replay_b1
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: fx.recovered_valid_end,
                    }
                })
                .expect("AC7: SegmentTruncateStart in recover_lane");
            assert!(
                pos_q_fsync_done < pos_trunc_start,
                "AC7 violated: QuarantineFsyncDirComplete ({pos_q_fsync_done}) must strictly precede SegmentTruncateStart ({pos_trunc_start})"
            );

            // AC1: 100% of seg 0 prefix + synced tail frames retained & lossless.
            assert_seg0_all_4_frames_intact(&fx, "rb08i-b1");
            assert_eq!(rec_b1.meta.active_seg, 1);
            assert_eq!(rec_b1.meta.committed_len, 0);
            assert_eq!(
                rec_b1.meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: fx.recovered_valid_end,
                    sealed_crc32c: fx.expected_recovered_crc,
                }]
            );

            // Also verify Boundary 1 + AC7 via the `append_frame` producer path.
            let fx_app = make_case_c_fixture("rb08i-b1-app");
            let incoming =
                encode_frame(FRAME_FLAG_NONE, "rb08i-b1-app-new", b"producer-after-b1").unwrap();
            let mut obs_app_crash = SegmentObserver::with_crash(
                SegmentCrashPoint::AfterQuarantineDirFsyncBeforeDestructive,
            );
            let _ = append_validated_frame_in_storage_dir(
                fx_app.repo.path(),
                CaptureQueueLane::Normal,
                &incoming,
                &mut obs_app_crash,
            )
            .expect_err("Boundary 1 producer crash must fail");
            assert_eq!(
                fs::read(&fx_app.seg0_path).unwrap(),
                fx_app.pre_crash_seg0_bytes
            );
            let mut obs_app_replay = SegmentObserver::new();
            let app_out = append_validated_frame_in_storage_dir(
                fx_app.repo.path(),
                CaptureQueueLane::Normal,
                &incoming,
                &mut obs_app_replay,
            )
            .expect("Boundary 1 producer replay must converge and append");
            let pos_app_q_fsync = obs_app_replay
                .events
                .iter()
                .position(|e| {
                    matches!(
                        e,
                        SegmentOpEvent::QuarantineFsyncDirComplete { id }
                            if id == &fx_app.expected_q_id
                    )
                })
                .expect("AC7: QuarantineFsyncDirComplete in append_frame");
            let pos_app_trunc = obs_app_replay
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: fx_app.recovered_valid_end,
                    }
                })
                .expect("AC7: SegmentTruncateStart in append_frame");
            assert!(
                pos_app_q_fsync < pos_app_trunc,
                "AC7 violated on producer path: fsync_dir(quarantine_dir) must precede truncate"
            );
            let q_app_entries: Vec<_> = fs::read_dir(&fx_app.init.paths.quarantine)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(
                q_app_entries,
                vec![std::ffi::OsString::from(format!(
                    "{}.bin",
                    fx_app.expected_q_id
                ))],
                "AC1 violated on producer replay: Boundary 1 must produce zero duplicate quarantine files"
            );
            assert_eq!(
                fs::read(&fx_app.expected_q_bin).unwrap(),
                fx_app.half_frame_tail
            );
            assert_seg0_all_4_frames_intact(&fx_app, "rb08i-b1-app");
            assert_eq!(app_out.seg_id, 1);
            assert_eq!(app_out.frame_offset, 0);
            let s1_decoded = decode_all_segment_bytes(&fs::read(&fx_app.seg1_path).unwrap());
            assert_eq!(s1_decoded.len(), 1);
            assert_eq!(s1_decoded[0].dedup_key, "rb08i-b1-app-new");
        }

        // =====================================================================
        // AC2 + AC3: Boundary 2 — crash strictly AFTER `truncate(S)+sync_data()`
        // and BEFORE Step C1 `LTM2` commit.
        // Restart detects clean segment `S` up to `recovered_valid_end` (zero
        // new quarantine), advances `LTM2.COMMITTED_LEN = recovered_valid_end`,
        // and keeps `LTM2.ACTIVE_SEG == S` (no `S+1`, `sealed[]` unchanged).
        // =====================================================================
        {
            let fx = make_case_c_fixture("rb08i-b2");
            let mut obs_crash_b2 = SegmentObserver::with_crash(
                SegmentCrashPoint::AfterCaseCStepC1TruncateSyncBeforeMeta,
            );
            let b2_err = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_crash_b2,
            )
            .expect_err("Boundary 2: crash after truncate+sync before Step C1 LTM2 must fail");
            assert_eq!(b2_err.stable_code_str(), "LBR-IO-001");

            // Post-crash disk state: seg 0 already cleanly truncated to
            // `recovered_valid_end`, quarantine `.bin` persisted, `LTM2` STILL
            // at `old_committed_len < recovered_valid_end`.
            assert_eq!(
                fs::metadata(&fx.seg0_path).unwrap().len(),
                fx.recovered_valid_end
            );
            assert_eq!(fs::read(&fx.expected_q_bin).unwrap(), fx.half_frame_tail);
            assert!(!fx.seg1_path.exists());
            let meta_before_restart = read_converged_lane_tail_meta(&fx.init.paths.normal).unwrap();
            assert_eq!(meta_before_restart.active_seg, 0);
            assert_eq!(meta_before_restart.committed_len, fx.old_committed_len);
            assert!(meta_before_restart.sealed.is_empty());

            // Restart converges via pure synced tail branch (`has_damaged_tail == false`).
            let mut obs_b2_restart = SegmentObserver::new();
            let rec_b2 = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_b2_restart,
            )
            .expect("AC2/AC3: Boundary 2 restart must converge cleanly");
            assert!(
                rec_b2.quarantined_artifacts.is_empty(),
                "Boundary 2 restart must perform zero new quarantine"
            );
            assert!(
                !obs_b2_restart.events.iter().any(|e| matches!(
                    e,
                    SegmentOpEvent::SegmentTruncateStart { .. }
                        | SegmentOpEvent::CreateSegment { .. }
                )),
                "Boundary 2 restart must not re-truncate or create S+1"
            );
            assert_eq!(rec_b2.recovered_frames, 2);
            assert_eq!(
                rec_b2.recovered_bytes,
                fx.recovered_valid_end - fx.old_committed_len
            );
            // AC2: `LTM2.COMMITTED_LEN == recovered_valid_end` (and matching CRC32C).
            let raw_meta = fs::read(tail_meta_path(&fx.init.paths.normal)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(&fx.init.paths.normal)).unwrap();
            assert_eq!(raw_meta, raw_bak);
            let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
            assert_eq!(disk_meta, rec_b2.meta);
            assert_eq!(
                disk_meta.committed_len, fx.recovered_valid_end,
                "AC2 violated: Boundary 2 restart must advance LTM2.COMMITTED_LEN to recovered_valid_end"
            );
            assert_eq!(disk_meta.prefix_crc32c, fx.expected_recovered_crc);
            // AC3: `LTM2.ACTIVE_SEG == S` (0), `sealed[]` empty, zero `S+1` on disk.
            assert_eq!(
                disk_meta.active_seg, 0,
                "AC3 violated: Boundary 2 restart must keep LTM2.ACTIVE_SEG == S"
            );
            assert!(disk_meta.sealed.is_empty());
            assert!(!fx.seg1_path.exists());
            assert_seg0_all_4_frames_intact(&fx, "rb08i-b2");

            // Subsequent append lands seamlessly on segment 0 at `recovered_valid_end`.
            let post_b2 = append_frame(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08i-b2-post",
                b"post-b2-append-on-s0",
            )
            .expect("append after Boundary 2 convergence must succeed on S");
            assert_eq!(post_b2.seg_id, 0);
            assert_eq!(post_b2.frame_offset, fx.recovered_valid_end);

            // Subcase: zero synced tail (`old_committed_len == recovered_valid_end < actual_len`)
            // crashing at Boundary 2 converges via Case A clean with `ACTIVE_SEG == S`.
            let z_repo = tempdir().unwrap();
            let z_init = init_capture_queue(z_repo.path()).unwrap();
            let z_p1 = append_frame(
                z_repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08i-b2z-p1",
                b"zero-tail-prefix",
            )
            .unwrap();
            let z_seg0 = z_init.paths.normal.join(segment_log_file_name(0));
            {
                let mut f = fs::OpenOptions::new().append(true).open(&z_seg0).unwrap();
                f.write_all(b"LCE1\x10\x00\x00\x00partial").unwrap();
                f.sync_data().unwrap();
            }
            let mut z_crash = SegmentObserver::with_crash(
                SegmentCrashPoint::AfterCaseCStepC1TruncateSyncBeforeMeta,
            );
            let _ =
                recover_lane_in_storage_dir(z_repo.path(), CaptureQueueLane::Normal, &mut z_crash)
                    .expect_err("zero-tail Boundary 2 crash must fail");
            let z_rec = recover_lane(z_repo.path(), CaptureQueueLane::Normal)
                .expect("zero-tail Boundary 2 restart must converge");
            assert_eq!(z_rec.meta.active_seg, 0);
            assert_eq!(z_rec.meta.committed_len, z_p1.meta.committed_len);
            assert!(z_rec.meta.sealed.is_empty());
            assert!(z_rec.quarantined_artifacts.is_empty());
            assert!(!z_init.paths.normal.join(segment_log_file_name(1)).exists());
        }

        // =====================================================================
        // AC4 + AC5 + AC6: Boundary 3 — crash strictly AFTER Step C1 commits
        // dual `LTM2` AND Step C2 creates empty `events-<S+1>.log` + `fsync_dir`,
        // BEFORE Step C2 commits dual `LTM2`.
        // Restart runs Case D empty orphan cleanup: unlinks empty `events-<S+1>.log`
        // (AC4), keeps `LTM2.ACTIVE_SEG == S` (AC5), and preserves authenticated
        // `LTM2.COMMITTED_LEN == recovered_valid_end` (AC6).
        // =====================================================================
        for via_append in [false, true] {
            let tag = if via_append {
                "rb08i-b3-app"
            } else {
                "rb08i-b3-rec"
            };
            let fx = make_case_c_fixture(tag);
            let mut obs_crash_b3 =
                SegmentObserver::with_crash(SegmentCrashPoint::AfterStepC2CreateSegmentBeforeMeta);
            let b3_err = recover_lane_in_storage_dir(
                fx.repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_crash_b3,
            )
            .expect_err("Boundary 3: crash after creating empty S+1 before Step C2 LTM2 must fail");
            assert_eq!(b3_err.stable_code_str(), "LBR-IO-001");

            // Post-crash disk state: empty `events-00000001.log` exists on disk,
            // while Step C1 dual `LTM2` persists `(ACTIVE_SEG=0, COMMITTED_LEN=recovered_valid_end)`.
            assert!(
                fx.seg1_path.exists(),
                "Boundary 3 post-crash: empty S+1 must exist before restart"
            );
            assert_eq!(fs::metadata(&fx.seg1_path).unwrap().len(), 0);
            let pre_b3_meta = read_converged_lane_tail_meta(&fx.init.paths.normal).unwrap();
            assert_eq!(pre_b3_meta.active_seg, 0);
            assert_eq!(pre_b3_meta.committed_len, fx.recovered_valid_end);
            assert_eq!(pre_b3_meta.prefix_crc32c, fx.expected_recovered_crc);
            assert!(pre_b3_meta.sealed.is_empty());

            if !via_append {
                let mut obs_b3_restart = SegmentObserver::new();
                let rec_b3 = recover_lane_in_storage_dir(
                    fx.repo.path(),
                    CaptureQueueLane::Normal,
                    &mut obs_b3_restart,
                )
                .expect("AC4–AC6: Boundary 3 recover_lane restart must converge via Case D");
                // AC4: Empty successor `events-00000001.log` unlinked + `fsync_dir(lane_dir)`.
                assert!(
                    !fx.seg1_path.exists(),
                    "AC4 violated: Case D must unlink empty successor events-00000001.log"
                );
                let pos_unlink_start = obs_b3_restart
                    .events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::OrphanUnlinkStart {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            seg_id: 1,
                        }
                    })
                    .expect("AC4: OrphanUnlinkStart(1)");
                let pos_unlink_fsync = obs_b3_restart
                    .events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            seg_id: 1,
                        }
                    })
                    .expect("AC4: FsyncLaneDirAfterOrphanUnlink(1)");
                assert!(pos_unlink_start < pos_unlink_fsync);
                assert!(rec_b3.quarantined_artifacts.is_empty());

                // AC5 + AC6: Dual `LTM2` preserves `ACTIVE_SEG == S` and `COMMITTED_LEN == recovered_valid_end`.
                let raw_meta = fs::read(tail_meta_path(&fx.init.paths.normal)).unwrap();
                let raw_bak = fs::read(tail_meta_bak_path(&fx.init.paths.normal)).unwrap();
                assert_eq!(raw_meta, raw_bak);
                let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
                assert_eq!(disk_meta, rec_b3.meta);
                assert_eq!(
                    disk_meta.active_seg, 0,
                    "AC5 violated: Boundary 3 recovery must keep LTM2.ACTIVE_SEG == S"
                );
                assert!(disk_meta.sealed.is_empty());
                assert_eq!(
                    disk_meta.committed_len, fx.recovered_valid_end,
                    "AC6 violated: Boundary 3 recovery must preserve LTM2.COMMITTED_LEN == recovered_valid_end"
                );
                assert_eq!(disk_meta.prefix_crc32c, fx.expected_recovered_crc);
                assert_seg0_all_4_frames_intact(&fx, tag);

                // Subsequent public `append_frame` writes seamlessly on seg 0.
                let post = append_frame(
                    fx.repo.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    "rb08i-b3-post",
                    b"post-b3-on-s0",
                )
                .expect("append after Boundary 3 recovery must succeed on S");
                assert_eq!(post.seg_id, 0);
                assert_eq!(post.frame_offset, fx.recovered_valid_end);
            } else {
                // Producer restart path after Boundary 3:
                // Case D unlinks empty `S+1` + `fsync_dir(lane_dir)` strictly
                // before writing the incoming frame, preserves `ACTIVE_SEG == 0`
                // and `COMMITTED_LEN == recovered_valid_end`, then appends
                // directly onto `seg 0` at offset `recovered_valid_end`.
                let post_payload = b"producer-restart-after-b3-on-s0".to_vec();
                let post_encoded =
                    encode_frame(FRAME_FLAG_NONE, "rb08i-b3-app-post", &post_payload).unwrap();
                let mut obs_b3_app = SegmentObserver::new();
                let post = append_validated_frame_in_storage_dir(
                    fx.repo.path(),
                    CaptureQueueLane::Normal,
                    &post_encoded,
                    &mut obs_b3_app,
                )
                .expect("AC4–AC6: producer append restart after Boundary 3 must converge");
                assert!(
                    !fx.seg1_path.exists(),
                    "AC4 violated on producer restart: empty S+1 must be unlinked"
                );
                let pos_unlink_start = obs_b3_app
                    .events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::OrphanUnlinkStart {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            seg_id: 1,
                        }
                    })
                    .expect("AC4 producer path: OrphanUnlinkStart(1)");
                let pos_unlink_fsync = obs_b3_app
                    .events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::FsyncLaneDirAfterOrphanUnlink {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            seg_id: 1,
                        }
                    })
                    .expect("AC4 producer path: FsyncLaneDirAfterOrphanUnlink(1)");
                let pos_write_s0 = obs_b3_app
                    .events
                    .iter()
                    .position(|e| {
                        *e == SegmentOpEvent::SegmentWriteAll {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            seg_id: 0,
                            offset: fx.recovered_valid_end,
                            len: post_encoded.len(),
                        }
                    })
                    .expect("producer path: incoming write_all on S");
                assert!(
                    pos_unlink_start < pos_unlink_fsync && pos_unlink_fsync < pos_write_s0,
                    "AC4 violated on producer restart: unlink(S+1) < fsync_dir(lane_dir) must precede incoming frame write"
                );
                assert_eq!(post.seg_id, 0);
                assert_eq!(post.frame_offset, fx.recovered_valid_end);
                assert_eq!(post.meta.active_seg, 0);
                assert!(post.meta.sealed.is_empty());
                assert_eq!(
                    post.meta.committed_len,
                    fx.recovered_valid_end + post.frame_len as u64
                );
                let decoded = decode_all_segment_bytes(&fs::read(&fx.seg0_path).unwrap());
                assert_eq!(decoded.len(), 5);
                assert_eq!(decoded[4].dedup_key, "rb08i-b3-app-post");
                assert_eq!(decoded[4].payload, post_payload);

                // Follow-up public `append_frame` succeeds seamlessly on seg 0.
                let follow_up = append_frame(
                    fx.repo.path(),
                    CaptureQueueLane::Normal,
                    FRAME_FLAG_NONE,
                    "rb08i-b3-app-followup",
                    b"followup-public-append-on-s0",
                )
                .expect("follow-up public append_frame on S must succeed");
                assert_eq!(follow_up.seg_id, 0);
                assert_eq!(follow_up.frame_offset, post.meta.committed_len);
            }
        }
    }

    /// `RB-08q` AC1–AC4: Fresh-segment first-frame 0-byte crash
    /// (`recovered_valid_end == 0`) guard before Step C2:
    /// - AC1: Step C1 durably quarantines `[0, actual_len)` into `quarantine/`
    ///   with diagnostic code `LBR-AGENT-050`.
    /// - AC2: Step C1 truncates `events-<S>.log` to exact `0` bytes.
    /// - AC3: Recovery skips Step C2 (`seal_recovered_segment_to_next`), keeping
    ///   `LTM2.ACTIVE_SEG == S` (no `events-<S+1>.log` created).
    /// - AC4: `LTM2.sealed.len()` strictly retains its original count (never
    ///   inserting a 0-byte sealed entry), verified on both `S = 0` (`sealed == []`),
    ///   `S = 1` (`sealed.len() == 1`), and direct public `append_frame` producer path.
    #[test]
    fn case_c_zero_byte_first_frame_crash_truncates_without_sealing() {
        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");

        // ------------------------------------------------------------------
        // Scenario 1: Fresh empty lane (`S = 0`, `sealed.len() == 0`) with a
        // truncated half-written first frame -> `recover_lane`.
        // ------------------------------------------------------------------
        let half_s0_src = encode_frame(
            FRAME_FLAG_NONE,
            "rb08q-s0-first-half",
            b"half-written-first-frame-on-fresh-seg0",
        )
        .unwrap();
        let half_s0_tail = half_s0_src[..half_s0_src.len() / 2].to_vec();
        let seg0_path = init.paths.normal.join(segment_log_file_name(0));
        let seg1_path = init.paths.normal.join(segment_log_file_name(1));
        write_bytes_0600(&seg0_path, &half_s0_tail);

        let q0_id = quarantine_artifact_id(CaptureQueueLane::Normal, 0, 0, &half_s0_tail);
        let q0_bin = init.paths.quarantine.join(format!("{q0_id}.bin"));

        let mut obs_s0 = SegmentObserver::new();
        let rec_s0 =
            recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs_s0)
                .expect("RB-08q Scenario 1: recover_lane on 0-byte first-frame crash must succeed");

        // AC1: `[0, actual_len)` durably quarantined to `quarantine/` with `LBR-AGENT-050`.
        assert_eq!(rec_s0.quarantined_artifacts.len(), 1);
        assert_eq!(rec_s0.quarantined_artifacts[0].id, q0_id);
        assert_eq!(
            rec_s0.quarantined_artifacts[0].diagnostic_code,
            QUARANTINE_DIAGNOSTIC_CODE
        );
        assert_eq!(
            rec_s0.quarantined_artifacts[0].quarantined_bytes,
            half_s0_tail.len() as u64
        );
        assert_eq!(
            fs::read(&q0_bin).expect("AC1: read quarantined first-frame bytes"),
            half_s0_tail
        );
        assert!(!init.paths.quarantine.join(format!("{q0_id}.tmp")).exists());

        // Strict ordering: `fsync_dir(quarantine_dir) < truncate(0) < seg.sync_data() < dual LTM2`
        // and ZERO `CreateSegment` event.
        let pos_q0_fsync = obs_s0
            .events
            .iter()
            .position(
                |e| matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &q0_id),
            )
            .expect("quarantine fsync_dir complete");
        let pos_trunc0 = obs_s0
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentTruncateStart {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                    to_len: 0,
                }
            })
            .expect("truncate seg 0 to 0");
        let pos_sync0 = obs_s0
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::SegmentSyncData {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 0,
                }
            })
            .expect("sync seg 0 after truncate");
        let pos_bak0 = obs_s0
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("Step C1 dual LTM2 commit");
        assert!(pos_q0_fsync < pos_trunc0 && pos_trunc0 < pos_sync0 && pos_sync0 < pos_bak0);
        assert!(
            !obs_s0
                .events
                .iter()
                .any(|e| matches!(e, SegmentOpEvent::CreateSegment { .. })),
            "AC3 violated: recovered_valid_end == 0 must skip Step C2 and never create S+1"
        );

        // AC2: `events-00000000.log` truncated to exact 0 bytes.
        assert_eq!(
            fs::metadata(&seg0_path).unwrap().len(),
            0,
            "AC2 violated: seg 0 must be truncated to exact 0 bytes"
        );
        // AC3 + AC4: Dual `LTM2` keeps `ACTIVE_SEG == 0`, `COMMITTED_LEN == 0`,
        // `PREFIX_CRC32C == 0`, `sealed.len() == 0`, and no `events-00000001.log`.
        assert!(!seg1_path.exists());
        let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
        let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
        assert_eq!(raw_meta, raw_bak);
        let meta_s0 = LaneTailMeta::decode(&raw_meta).unwrap();
        assert_eq!(meta_s0, rec_s0.meta);
        assert_eq!(
            meta_s0.active_seg, 0,
            "AC3 violated: ACTIVE_SEG must stay 0"
        );
        assert_eq!(meta_s0.committed_len, 0);
        assert_eq!(meta_s0.prefix_crc32c, 0);
        assert_eq!(
            meta_s0.sealed.len(),
            0,
            "AC4 violated: sealed.len() must remain 0"
        );

        // Follow-up append writes cleanly onto seg 0 at offset 0.
        let s0_payload = b"valid-frame-on-seg0-after-zero-byte-recovery".to_vec();
        let out_s0 = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08q-s0-ok",
            &s0_payload,
        )
        .expect("append on seg 0 after 0-byte recovery must succeed");
        assert_eq!(out_s0.seg_id, 0);
        assert_eq!(out_s0.frame_offset, 0);

        // ------------------------------------------------------------------
        // Scenario 2: Non-zero active segment `S = 1` with existing
        // `sealed.len() == 1` (after a prior non-empty Step C2 rotation left
        // `events-00000001.log` empty), first frame on `S = 1` crashes
        // half-written -> `recover_lane` preserves `ACTIVE_SEG == 1` and
        // `sealed.len() == 1` (zero new sealed entry!).
        // ------------------------------------------------------------------
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&seg0_path)
                .unwrap();
            f.write_all(b"LCE1\x20\x00\x00\x00trigger-nonempty-c2")
                .unwrap();
            f.sync_data().unwrap();
        }
        let rot = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("rotate non-empty seg 0 via Step C2 to produce empty seg 1");
        assert_eq!(rot.meta.active_seg, 1);
        assert_eq!(rot.meta.committed_len, 0);
        assert_eq!(rot.meta.sealed.len(), 1);
        let original_sealed = rot.meta.sealed.clone();
        assert_eq!(fs::metadata(&seg1_path).unwrap().len(), 0);

        let half_s1_src = encode_frame(
            FRAME_FLAG_NONE,
            "rb08q-s1-first-half",
            b"half-written-first-frame-on-seg1",
        )
        .unwrap();
        let half_s1_tail = half_s1_src[..half_s1_src.len() - 7].to_vec();
        write_bytes_0600(&seg1_path, &half_s1_tail);
        let q1_id = quarantine_artifact_id(CaptureQueueLane::Normal, 1, 0, &half_s1_tail);
        let seg2_path = init.paths.normal.join(segment_log_file_name(2));

        let rec_s1 = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("RB-08q Scenario 2: recover_lane on seg 1 0-byte crash must succeed");
        assert_eq!(rec_s1.quarantined_artifacts.len(), 1);
        assert_eq!(rec_s1.quarantined_artifacts[0].id, q1_id);
        assert_eq!(
            fs::read(init.paths.quarantine.join(format!("{q1_id}.bin"))).unwrap(),
            half_s1_tail
        );
        assert_eq!(fs::metadata(&seg1_path).unwrap().len(), 0);
        assert!(
            !seg2_path.exists(),
            "AC3 violated on S=1: must not create seg 2"
        );
        assert_eq!(rec_s1.meta.active_seg, 1);
        assert_eq!(rec_s1.meta.committed_len, 0);
        assert_eq!(rec_s1.meta.prefix_crc32c, 0);
        assert_eq!(
            rec_s1.meta.sealed, original_sealed,
            "AC4 violated on S=1: sealed[] must stay strictly unchanged at len 1"
        );

        // ------------------------------------------------------------------
        // Scenario 3: Direct public `append_frame` producer path when empty
        // `S = 1` (`sealed.len() == 1`) has a corrupt first half-frame:
        // Step C1 quarantines & truncates `S = 1` to 0, skips Step C2, and
        // appends the incoming frame cleanly onto `S = 1` at offset 0.
        // ------------------------------------------------------------------
        let corrupt_s1_first = b"BAD!\xff\xff\xff\xffcorrupt-first-frame-s1".to_vec();
        write_bytes_0600(&seg1_path, &corrupt_s1_first);
        let q2_id = quarantine_artifact_id(CaptureQueueLane::Normal, 1, 0, &corrupt_s1_first);

        let app_payload = b"public-append-after-zero-byte-guard-on-s1".to_vec();
        let app_out = append_frame(
            repo.path(),
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb08q-s1-app",
            &app_payload,
        )
        .expect("RB-08q Scenario 3: public append_frame after 0-byte guard must succeed");
        assert_eq!(
            fs::read(init.paths.quarantine.join(format!("{q2_id}.bin"))).unwrap(),
            corrupt_s1_first
        );
        assert!(!seg2_path.exists());
        assert_eq!(app_out.seg_id, 1);
        assert_eq!(app_out.frame_offset, 0);
        assert_eq!(app_out.meta.active_seg, 1);
        assert_eq!(app_out.meta.sealed, original_sealed);
        assert_eq!(app_out.meta.committed_len, app_out.frame_len as u64);
        let s1_decoded = decode_all_segment_bytes(&fs::read(&seg1_path).unwrap());
        assert_eq!(s1_decoded.len(), 1);
        assert_eq!(s1_decoded[0].dedup_key, "rb08q-s1-app");
        assert_eq!(s1_decoded[0].payload, app_payload);
        // Defense-in-depth: `build_case_c_step_c2_meta` itself rejects a 0-byte segment.
        assert!(build_case_c_step_c2_meta(&rec_s1.meta).is_err());
    }

    /// `RB-08k` AC1–AC6: Nested Case C→D old-segment clean synced tail recovery
    /// (`max_seg > active_seg && old_valid_end == actual_len(S) > committed_len`):
    /// - AC1: Flipping 1 byte inside old segment `S` `[0, committed_len)` fails
    ///   closed with `CorruptCommittedFrame` (`LBR-IO-001`) strictly BEFORE
    ///   touching successor `S + 1` (zero disk mutation).
    /// - AC2: Seals old segment `S` into dual `LTM2.sealed[]` with exact grown
    ///   boundary `(S, old_valid_end, old_sealed_crc)` (NEVER stale `committed_len`).
    /// - AC3: Advances dual `LTM2.ACTIVE_SEG` to `S + 1`.
    /// - AC4: Sets dual `LTM2.COMMITTED_LEN` to exact valid frame bytes in `S + 1`.
    /// - AC5: Sets dual `LTM2.PREFIX_CRC32C` to exact `crc32c(events-<S+1>[0..committed_len])`.
    /// - AC6: Preserves `events-<S+1>` bytes verbatim (`actual_s1_bytes == expected_s1_bytes`),
    ///   verified across `recover_lane`, direct public `append_frame`, nested
    ///   `committed_len == 0 < old_valid_end`, and nested clean old tail + orphan `S + 1`.
    #[test]
    fn nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary() {
        // =====================================================================
        // Part 1 — AC1: Nested state old-segment `[0, committed_len)` prefix
        // single-byte bitflip MUST fail closed BEFORE touching `S + 1` (verified
        // both with a valid `S + 1` and with an unreadable/symlink `S + 1` so
        // `CorruptCommittedFrame` fires before `S + 1` is ever opened!).
        // =====================================================================
        {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08k-ac1-p1",
                b"committed-prefix-body",
            )
            .unwrap();
            let old_tail_frame = encode_frame(
                FRAME_FLAG_NONE,
                "rb08k-ac1-old-tail",
                b"synced-old-tail-frame",
            )
            .unwrap();
            let s1_frame =
                encode_frame(FRAME_FLAG_NONE, "rb08k-ac1-s1", b"valid-successor-frame").unwrap();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));

            let mut corrupted_seg0 = fs::read(&seg0_path).unwrap();
            assert_eq!(corrupted_seg0.len() as u64, p1.meta.committed_len);
            let flip_idx = corrupted_seg0.len() / 2;
            corrupted_seg0[flip_idx] ^= 0x80; // flip 1 byte in [0, committed_len)
            corrupted_seg0.extend_from_slice(&old_tail_frame);
            write_bytes_0600(&seg0_path, &corrupted_seg0);
            write_bytes_0600(&seg1_path, &s1_frame);

            let snap_before = snapshot_tree(&init.paths.root);
            let err = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect_err("AC1: nested old-segment prefix bitflip must fail closed");
            assert!(matches!(
                err,
                StorageBackendError::CorruptCommittedFrame { .. }
            ));
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(snapshot_tree(&init.paths.root), snap_before);

            // Strengthen AC1 ordering proof on Unix: replace `S + 1` with a
            // symlink; old-segment prefix check STILL fires `CorruptCommittedFrame`
            // (not symlink `BackendInternal`) proving `S + 1` is never opened first.
            #[cfg(unix)]
            {
                fs::remove_file(&seg1_path).unwrap();
                std::os::unix::fs::symlink(&seg0_path, &seg1_path).unwrap();
                let err_sym = recover_lane(repo.path(), CaptureQueueLane::Normal)
                    .expect_err("AC1: old-segment prefix check must precede opening S+1");
                assert!(matches!(
                    err_sym,
                    StorageBackendError::CorruptCommittedFrame { .. }
                ));
                assert_eq!(err_sym.stable_code_str(), "LBR-IO-001");
            }
        }

        // =====================================================================
        // Part 2 — AC2–AC6 (`recover_lane` path): old segment `S = 0` has
        // committed prefix + 2 complete synced uncommitted tail frames (incl.
        // >64 KiB frame), and successor `S = 1` has 2 complete synced valid frames.
        // =====================================================================
        {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let p1_payload = b"rb08k-s0-prefix-1".to_vec();
            let p2_payload = b"rb08k-s0-prefix-2".to_vec();
            let _out_p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08k-p1",
                &p1_payload,
            )
            .unwrap();
            let out_p2 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08k-p2",
                &p2_payload,
            )
            .unwrap();
            let stale_committed_len = out_p2.meta.committed_len;
            let stale_prefix_crc = out_p2.meta.prefix_crc32c;

            // Append 2 complete synced uncommitted tail frames to old seg 0.
            let ot1_payload = b"rb08k-old-synced-tail-1".to_vec();
            let ot2_payload = vec![0x3cu8; 70 * 1024];
            let ot1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08k-ot1", &ot1_payload).unwrap();
            let ot2_bytes = encode_frame(FRAME_FLAG_NONE, "rb08k-ot2-70k", &ot2_payload).unwrap();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&ot1_bytes).unwrap();
                f.write_all(&ot2_bytes).unwrap();
                f.sync_data().unwrap();
            }
            let old_valid_end =
                stale_committed_len + ot1_bytes.len() as u64 + ot2_bytes.len() as u64;
            assert!(old_valid_end > stale_committed_len);
            let old_sealed_crc =
                crc32c_append(crc32c_append(stale_prefix_crc, &ot1_bytes), &ot2_bytes);
            assert_eq!(
                crc32c_checksum(&fs::read(&seg0_path).unwrap()),
                old_sealed_crc
            );

            // Create valid successor `events-00000001.log` with 2 complete frames.
            let s1_f1_payload = b"rb08k-s1-frame-1".to_vec();
            let s1_f2_payload = vec![0x91u8; 68 * 1024];
            let s1_f1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08k-s1-f1", &s1_f1_payload).unwrap();
            let s1_f2_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08k-s1-f2-68k", &s1_f2_payload).unwrap();
            let mut expected_s1_bytes = Vec::with_capacity(s1_f1_bytes.len() + s1_f2_bytes.len());
            expected_s1_bytes.extend_from_slice(&s1_f1_bytes);
            expected_s1_bytes.extend_from_slice(&s1_f2_bytes);
            let expected_s1_len = expected_s1_bytes.len() as u64;
            let expected_s1_crc = crc32c_checksum(&expected_s1_bytes);
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            write_bytes_0600(&seg1_path, &expected_s1_bytes);

            let rec = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect("RB-08k: nested Case C->D clean old tail recovery must succeed");
            assert!(rec.quarantined_artifacts.is_empty());
            assert_eq!(rec.recovered_frames, 4); // 2 old-tail + 2 successor frames
            assert_eq!(
                rec.recovered_bytes,
                (old_valid_end - stale_committed_len) + expected_s1_len
            );

            // Dual on-disk `LTM2` replicas agree byte-for-byte.
            let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
            assert_eq!(raw_meta, raw_bak);
            let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
            assert_eq!(disk_meta, rec.meta);

            // AC2: Old segment `S = 0` sealed with exact grown boundary
            // `(0, old_valid_end, old_sealed_crc)`, NEVER stale `committed_len`.
            assert_eq!(
                disk_meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: old_valid_end,
                    sealed_crc32c: old_sealed_crc,
                }],
                "AC2 violated: LTM2.sealed[0] must record exact grown (old_valid_end, old_sealed_crc)"
            );
            assert_ne!(disk_meta.sealed[0].sealed_len, stale_committed_len);

            // AC3: `ACTIVE_SEG == 1` (`S + 1`).
            assert_eq!(
                disk_meta.active_seg, 1,
                "AC3 violated: LTM2.ACTIVE_SEG must advance to S + 1"
            );
            // AC4: `COMMITTED_LEN == expected_s1_len`.
            assert_eq!(
                disk_meta.committed_len, expected_s1_len,
                "AC4 violated: LTM2.COMMITTED_LEN must equal successor valid bytes"
            );
            // AC5: `PREFIX_CRC32C == crc32c(events-00000001.log[0..committed_len])`.
            assert_eq!(
                disk_meta.prefix_crc32c, expected_s1_crc,
                "AC5 violated: LTM2.PREFIX_CRC32C must match successor CRC32C"
            );
            // AC6: `events-00000001.log` full bytes preserved verbatim + lossless decode.
            let actual_s1_bytes = fs::read(&seg1_path).expect("read recovered seg1");
            assert_eq!(
                actual_s1_bytes, expected_s1_bytes,
                "AC6 violated: successor events-<S+1> bytes must be preserved verbatim"
            );
            let s0_decoded = decode_all_segment_bytes(&fs::read(&seg0_path).unwrap());
            assert_eq!(
                s0_decoded
                    .iter()
                    .map(|f| f.dedup_key.as_str())
                    .collect::<Vec<_>>(),
                vec!["rb08k-p1", "rb08k-p2", "rb08k-ot1", "rb08k-ot2-70k"]
            );
            assert_eq!(s0_decoded[0].flags, FRAME_FLAG_NONE);
            assert_eq!(s0_decoded[0].payload, p1_payload);
            assert_eq!(s0_decoded[1].flags, FRAME_FLAG_NONE);
            assert_eq!(s0_decoded[1].payload, p2_payload);
            assert_eq!(s0_decoded[2].flags, FRAME_FLAG_NONE);
            assert_eq!(s0_decoded[2].payload, ot1_payload);
            assert_eq!(s0_decoded[3].flags, FRAME_FLAG_NONE);
            assert_eq!(s0_decoded[3].payload, ot2_payload);
            let s1_decoded = decode_all_segment_bytes(&actual_s1_bytes);
            assert_eq!(
                s1_decoded
                    .iter()
                    .map(|f| f.dedup_key.as_str())
                    .collect::<Vec<_>>(),
                vec!["rb08k-s1-f1", "rb08k-s1-f2-68k"]
            );
            assert_eq!(s1_decoded[0].payload, s1_f1_payload);
            assert_eq!(s1_decoded[1].payload, s1_f2_payload);

            // =================================================================
            // Part 3 — Direct public `append_frame` producer path on nested
            // clean old tail on `S = 1` + valid `S = 2` (verifies multi-generation
            // sealing and seamless append at `S = 2` offset `expected_s2_len`).
            // =================================================================
            let ot_s1_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08k-s1-ot", b"old-tail-on-seg1").unwrap();
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg1_path)
                    .unwrap();
                f.write_all(&ot_s1_bytes).unwrap();
                f.sync_data().unwrap();
            }
            let expected_s1_sealed_bytes = fs::read(&seg1_path).unwrap();
            let s2_init_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08k-s2-init", b"successor-s2-frame").unwrap();
            let seg2_path = init.paths.normal.join(segment_log_file_name(2));
            write_bytes_0600(&seg2_path, &s2_init_bytes);

            let s2_new_payload = b"producer-appended-onto-s2-after-nested-cd".to_vec();
            let out_s2 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08k-s2-new",
                &s2_new_payload,
            )
            .expect("public append_frame after nested Case C->D must succeed");
            assert_eq!(out_s2.seg_id, 2);
            assert_eq!(out_s2.frame_offset, s2_init_bytes.len() as u64);
            assert_eq!(out_s2.recovered_frames, 2);
            assert_eq!(out_s2.meta.active_seg, 2);
            assert_eq!(out_s2.meta.sealed.len(), 2);
            assert_eq!(
                out_s2.meta.sealed[1],
                SealedSegmentMeta {
                    seg_id: 1,
                    sealed_len: expected_s1_sealed_bytes.len() as u64,
                    sealed_crc32c: crc32c_checksum(&expected_s1_sealed_bytes),
                }
            );
            let s2_decoded = decode_all_segment_bytes(&fs::read(&seg2_path).unwrap());
            assert_eq!(s2_decoded.len(), 2);
            assert_eq!(s2_decoded[0].dedup_key, "rb08k-s2-init");
            assert_eq!(s2_decoded[1].dedup_key, "rb08k-s2-new");
            assert_eq!(s2_decoded[1].payload, s2_new_payload);
        }

        // =====================================================================
        // Part 4 — Edge cases:
        // (a) `committed_len == 0 < old_valid_end` (fresh seg 0 gained complete
        //     synced frame before valid `S = 1` appeared -> sealed accurately);
        // (b) Nested clean old tail + empty orphan `S + 1` -> unlinks orphan
        //     and advances `LTM2` on `S` to `(S, old_valid_end, old_sealed_crc)`.
        // =====================================================================
        {
            let repo = tempdir().unwrap();
            let init = init_capture_queue(repo.path()).unwrap();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            let s0_frame = encode_frame(FRAME_FLAG_NONE, "rb08k-z-s0", b"zero-base-s0").unwrap();
            let s1_frame = encode_frame(FRAME_FLAG_NONE, "rb08k-z-s1", b"zero-base-s1").unwrap();
            write_bytes_0600(&seg0_path, &s0_frame);
            write_bytes_0600(&seg1_path, &s1_frame);

            let rec = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect("nested committed_len == 0 < old_valid_end must seal accurately");
            assert_eq!(rec.meta.active_seg, 1);
            assert_eq!(
                rec.meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: s0_frame.len() as u64,
                    sealed_crc32c: crc32c_checksum(&s0_frame),
                }]
            );
            assert_eq!(rec.meta.committed_len, s1_frame.len() as u64);
            assert_eq!(fs::read(&seg1_path).unwrap(), s1_frame);

            // (b) Nested clean old tail on `S = 1` + empty orphan `S = 2`.
            let s1_ot = encode_frame(FRAME_FLAG_NONE, "rb08k-z-s1-ot", b"s1-tail-orphan").unwrap();
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg1_path)
                    .unwrap();
                f.write_all(&s1_ot).unwrap();
                f.sync_data().unwrap();
            }
            let seg2_path = init.paths.normal.join(segment_log_file_name(2));
            write_bytes_0600(&seg2_path, b"");
            let expected_s1_full = fs::read(&seg1_path).unwrap();

            let rec_orp = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect("nested clean old tail + empty orphan S+1 must converge cleanly");
            assert!(!seg2_path.exists());
            assert_eq!(rec_orp.meta.active_seg, 1);
            assert_eq!(rec_orp.meta.sealed.len(), 1);
            assert_eq!(rec_orp.meta.committed_len, expected_s1_full.len() as u64);
            assert_eq!(
                rec_orp.meta.prefix_crc32c,
                crc32c_checksum(&expected_s1_full)
            );
            assert_eq!(rec_orp.recovered_frames, 1);
        }

        // =====================================================================
        // Part 5 — Structural guards & precedence:
        // (a) 0-byte effective old segment (`committed_len == 0 && old_valid_end == 0 <
        //     actual_len(S)`) + valid `S + 1` rejects in Phase 1 with
        //     `CorruptCommittedFrame` (`LBR-IO-001`) and zero whole-tree mutation;
        // (b) Full 256-entry `sealed[]` + damaged nested old tail + valid `S + 1`
        //     rejects in Phase 1 with `CorruptCommittedFrame` (`LBR-IO-001`) and
        //     zero whole-tree mutation.
        // =====================================================================
        {
            let repo = tempdir().unwrap();
            let init = init_capture_queue(repo.path()).unwrap();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            let bad_src =
                encode_frame(FRAME_FLAG_NONE, "rb08k-bad", b"truncated-old-tail").unwrap();
            write_bytes_0600(&seg0_path, &bad_src[..bad_src.len() / 2]);
            let s1_valid = encode_frame(FRAME_FLAG_NONE, "rb08k-guard-s1", b"valid-s1").unwrap();
            write_bytes_0600(&seg1_path, &s1_valid);

            let snap_before_guard = snapshot_tree(&init.paths.root);
            let err_guard = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect_err("0-byte effective old segment + valid S+1 must fail closed in Phase 1");
            assert!(matches!(
                err_guard,
                StorageBackendError::CorruptCommittedFrame { .. }
            ));
            assert_eq!(err_guard.stable_code_str(), "LBR-IO-001");
            assert_eq!(snapshot_tree(&init.paths.root), snap_before_guard);

            // (b) Full 256-entry `sealed[]` rejects before any nested old-tail recovery.
            let full_repo = tempdir().unwrap();
            let full_init = init_capture_queue(full_repo.path()).unwrap();
            let active_seg_id: u32 = MAX_SEALED_SEGMENTS as u32;
            let active_seg_path = full_init
                .paths
                .normal
                .join(segment_log_file_name(active_seg_id));
            let succ_seg_path = full_init
                .paths
                .normal
                .join(segment_log_file_name(active_seg_id + 1));
            let prefix_frame =
                encode_frame(FRAME_FLAG_NONE, "rb08k-full-p", b"full-table-prefix").unwrap();
            let mut active_bytes = prefix_frame.clone();
            active_bytes.extend_from_slice(&bad_src[..bad_src.len() / 2]);
            write_bytes_0600(&active_seg_path, &active_bytes);
            write_bytes_0600(&succ_seg_path, &s1_valid);

            let sealed: Vec<SealedSegmentMeta> = (0..active_seg_id)
                .map(|seg_id| SealedSegmentMeta {
                    seg_id,
                    sealed_len: 32,
                    sealed_crc32c: 0x1234_5678,
                })
                .collect();
            let full_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: active_seg_id,
                committed_len: prefix_frame.len() as u64,
                prefix_crc32c: crc32c_checksum(&prefix_frame),
                sealed,
            };
            let encoded_full = full_meta.encode().unwrap();
            write_bytes_0600(&tail_meta_path(&full_init.paths.normal), &encoded_full);
            write_bytes_0600(&tail_meta_bak_path(&full_init.paths.normal), &encoded_full);

            let snap_full = snapshot_tree(&full_init.paths.root);
            let err_full = recover_lane(full_repo.path(), CaptureQueueLane::Normal)
                .expect_err("full 256-entry table must fail closed before nested tail recovery");
            assert!(matches!(
                err_full,
                StorageBackendError::CorruptCommittedFrame { .. }
            ));
            assert_eq!(err_full.stable_code_str(), "LBR-IO-001");
            assert_eq!(snapshot_tree(&full_init.paths.root), snap_full);
        }
    }

    /// `RB-08v` AC1–AC3, AC7, AC8: Nested Case C→D when old segment `S` has
    /// complete uncommitted synced tail frames followed by a damaged tail
    /// (`committed_len < old_valid_end < actual_len(S)`):
    /// - AC8: Crash strictly before old-tail quarantine `rename` leaves
    ///   `events-<S>.log` byte-for-byte identical (zero premature truncation).
    /// - AC7: Observer sequence proves `QuarantineFsyncDirComplete < SegmentTruncateStart`
    ///   on BOTH `recover_lane` and producer `append_frame` paths (including when
    ///   both `S` and `S + 1` carry damaged tails).
    /// - AC1: `quarantine/<id>.bin` equals exact damaged bytes `[old_valid_end, actual_len(S))`
    ///   with diagnostic code `LBR-AGENT-050`.
    /// - AC2: `events-<S>.log` is truncated in-place to exact `[0..old_valid_end)`.
    /// - AC3: Dual `LTM2.sealed[]` seals exact `(S, old_valid_end, old_sealed_crc)`
    ///   and advances to `S + 1`.
    #[test]
    fn nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal() {
        // =====================================================================
        // Part 1 — AC8 + AC1 + AC2 + AC3 + AC7 (`recover_lane` path)
        // =====================================================================
        {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let p1_payload = b"rb08v-p1-body".to_vec();
            let out_p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08v-p1",
                &p1_payload,
            )
            .unwrap();
            let old_committed_len = out_p1.meta.committed_len;
            let old_prefix_crc = out_p1.meta.prefix_crc32c;

            let ot1_payload = b"rb08v-old-tail-1".to_vec();
            let ot2_payload = vec![0x5du8; 70 * 1024];
            let ot1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08v-ot1", &ot1_payload).unwrap();
            let ot2_bytes = encode_frame(FRAME_FLAG_NONE, "rb08v-ot2-70k", &ot2_payload).unwrap();
            let bad_src =
                encode_frame(FRAME_FLAG_NONE, "rb08v-old-half", &vec![0x7eu8; 1024]).unwrap();
            let old_bad_tail = bad_src[..bad_src.len() / 2].to_vec();

            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&ot1_bytes).unwrap();
                f.write_all(&ot2_bytes).unwrap();
                f.write_all(&old_bad_tail).unwrap();
                f.sync_data().unwrap();
            }
            let pre_crash_seg0_bytes = fs::read(&seg0_path).unwrap();
            let old_valid_end = old_committed_len + ot1_bytes.len() as u64 + ot2_bytes.len() as u64;
            let expected_seg0_clean = pre_crash_seg0_bytes[..old_valid_end as usize].to_vec();
            let old_sealed_crc =
                crc32c_append(crc32c_append(old_prefix_crc, &ot1_bytes), &ot2_bytes);
            assert_eq!(crc32c_checksum(&expected_seg0_clean), old_sealed_crc);

            let s1_payload = b"rb08v-s1-valid-frame".to_vec();
            let s1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08v-s1-f1", &s1_payload).unwrap();
            write_bytes_0600(&seg1_path, &s1_bytes);

            let old_q_id =
                quarantine_artifact_id(CaptureQueueLane::Normal, 0, old_valid_end, &old_bad_tail);
            let old_q_bin = init.paths.quarantine.join(format!("{old_q_id}.bin"));
            let old_q_tmp = init.paths.quarantine.join(format!("{old_q_id}.tmp"));
            let meta_before = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
            let bak_before = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();

            // AC8: Crash strictly BEFORE old-tail quarantine `rename` leaves
            // `events-00000000.log` byte-for-byte identical (zero premature truncation).
            let mut obs_pre_rename =
                SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
            let err_pre_rename = recover_lane_in_storage_dir(
                repo.path(),
                CaptureQueueLane::Normal,
                &mut obs_pre_rename,
            )
            .expect_err("AC8: crash before old-tail quarantine rename must fail");
            assert_eq!(err_pre_rename.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&seg0_path).unwrap(),
                pre_crash_seg0_bytes,
                "AC8 violated: events-00000000.log must be byte-for-byte identical before rename"
            );
            assert_eq!(fs::read(&seg1_path).unwrap(), s1_bytes);
            assert_eq!(
                fs::read(tail_meta_path(&init.paths.normal)).unwrap(),
                meta_before
            );
            assert_eq!(
                fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap(),
                bak_before
            );
            assert!(old_q_tmp.exists());
            assert!(!old_q_bin.exists());
            assert!(
                !obs_pre_rename
                    .events
                    .iter()
                    .any(|e| matches!(e, SegmentOpEvent::SegmentTruncateStart { .. }))
            );

            // Clean restart recovers nested old damaged tail and seals `(0, old_valid_end, old_sealed_crc)`.
            let mut obs = SegmentObserver::new();
            let rec = recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs)
                .expect("RB-08v: nested damaged old-tail recovery must succeed");

            // AC1: Exact damaged tail bytes published to `quarantine/<id>.bin` with `LBR-AGENT-050`.
            assert_eq!(rec.cleaned_quarantine_tmp_files, 1);
            assert_eq!(rec.quarantined_artifacts.len(), 1);
            assert_eq!(rec.quarantined_artifacts[0].id, old_q_id);
            assert_eq!(
                rec.quarantined_artifacts[0].diagnostic_code,
                QUARANTINE_DIAGNOSTIC_CODE
            );
            assert_eq!(
                fs::read(&old_q_bin).expect("AC1: read old-tail quarantine .bin"),
                old_bad_tail
            );
            assert!(!old_q_tmp.exists());

            // AC7: Observer proves `fsync_dir(quarantine_dir) < truncate(seg 0) < seg0.sync_data() < dual LTM2`.
            let pos_q_fsync = obs
                .events
                .iter()
                .position(|e| {
                    matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &old_q_id)
                })
                .expect("AC7: QuarantineFsyncDirComplete(old_q_id)");
            let pos_trunc_start = obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: old_valid_end,
                    }
                })
                .expect("AC7: SegmentTruncateStart(seg 0)");
            let pos_trunc_done = obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateComplete {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: old_valid_end,
                    }
                })
                .expect("AC7: SegmentTruncateComplete(seg 0)");
            let pos_seg_sync = obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentSyncData {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                    }
                })
                .expect("AC7: SegmentSyncData(seg 0)");
            let pos_case_d = obs
                .events
                .iter()
                .position(|e| {
                    matches!(
                        e,
                        SegmentOpEvent::CaseDRecoverCleanSuccessor {
                            lane: QUEUE_LANE_NORMAL_DIR,
                            old_seg_id: 0,
                            succ_seg_id: 1,
                            ..
                        }
                    )
                })
                .expect("CaseDRecoverSuccessor(0 -> 1)");
            assert!(
                pos_q_fsync < pos_trunc_start
                    && pos_trunc_start < pos_trunc_done
                    && pos_trunc_done < pos_seg_sync
                    && pos_seg_sync < pos_case_d,
                "AC7 violated: quarantine fsync_dir < truncate < sync_data < Case D seal"
            );

            // AC2: `events-00000000.log` exact bytes == `[0..old_valid_end)`.
            let seg0_after = fs::read(&seg0_path).unwrap();
            assert_eq!(
                seg0_after, expected_seg0_clean,
                "AC2 violated: events-00000000.log must equal exact [0..old_valid_end)"
            );
            let seg0_decoded = decode_all_segment_bytes(&seg0_after);
            assert_eq!(seg0_decoded.len(), 3);
            assert_eq!(seg0_decoded[0].payload, p1_payload);
            assert_eq!(seg0_decoded[1].payload, ot1_payload);
            assert_eq!(seg0_decoded[2].payload, ot2_payload);

            // AC3: Dual `LTM2.sealed[]` seals `(0, old_valid_end, old_sealed_crc)`.
            let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
            assert_eq!(raw_meta, raw_bak);
            let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
            assert_eq!(disk_meta, rec.meta);
            assert_eq!(
                disk_meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: old_valid_end,
                    sealed_crc32c: old_sealed_crc,
                }],
                "AC3 violated: LTM2.sealed[0] must record (0, old_valid_end, old_sealed_crc)"
            );
            assert_eq!(disk_meta.active_seg, 1);
            assert_eq!(disk_meta.committed_len, s1_bytes.len() as u64);
            assert_eq!(disk_meta.prefix_crc32c, crc32c_checksum(&s1_bytes));
            assert_eq!(fs::read(&seg1_path).unwrap(), s1_bytes);
        }

        // =====================================================================
        // Part 2 — Double-damaged nested Case C→D (damaged old tail on `S = 0`
        // AND trailing half-frame on valid `S = 1`) via producer `append_frame`
        // path: verifies AC1–AC3 + AC7 for BOTH segments plus new append on `S = 1`.
        // =====================================================================
        {
            let repo = tempdir().unwrap();
            let init = init_capture_queue(repo.path()).unwrap();
            let p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08v-dbl-p1",
                b"dbl-prefix",
            )
            .unwrap();
            let ot_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08v-dbl-ot", b"dbl-old-synced-tail").unwrap();
            let old_bad = b"LCE1\x20\x00\x00\x00old-seg-bad-tail".to_vec();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&ot_bytes).unwrap();
                f.write_all(&old_bad).unwrap();
                f.sync_data().unwrap();
            }
            let old_valid_end = p1.meta.committed_len + ot_bytes.len() as u64;
            let old_sealed_crc = crc32c_append(p1.meta.prefix_crc32c, &ot_bytes);
            let old_q_id =
                quarantine_artifact_id(CaptureQueueLane::Normal, 0, old_valid_end, &old_bad);

            let s1_valid =
                encode_frame(FRAME_FLAG_NONE, "rb08v-dbl-s1", b"dbl-succ-valid").unwrap();
            let succ_bad = b"BAD!\xff\xff\xff\xffsucc-bad-tail".to_vec();
            let mut s1_with_bad = s1_valid.clone();
            s1_with_bad.extend_from_slice(&succ_bad);
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            write_bytes_0600(&seg1_path, &s1_with_bad);
            let succ_q_id = quarantine_artifact_id(
                CaptureQueueLane::Normal,
                1,
                s1_valid.len() as u64,
                &succ_bad,
            );

            let incoming_payload = b"producer-frame-after-double-damaged-nested-cd".to_vec();
            let incoming_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08v-dbl-new", &incoming_payload).unwrap();
            let mut app_obs = SegmentObserver::new();
            let out = append_validated_frame_in_storage_dir(
                repo.path(),
                CaptureQueueLane::Normal,
                &incoming_bytes,
                &mut app_obs,
            )
            .expect("producer append on double-damaged nested Case C->D must succeed");

            // AC7 for BOTH old segment 0 and successor segment 1.
            let p_q0 = app_obs
                .events
                .iter()
                .position(|e| {
                    matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &old_q_id)
                })
                .expect("old seg quarantine fsync_dir");
            let p_t0 = app_obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: old_valid_end,
                    }
                })
                .expect("old seg truncate start");
            let p_q1 = app_obs
                .events
                .iter()
                .position(|e| {
                    matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &succ_q_id)
                })
                .expect("succ seg quarantine fsync_dir");
            let p_t1 = app_obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 1,
                        to_len: s1_valid.len() as u64,
                    }
                })
                .expect("succ seg truncate start");
            let p_w1 = app_obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentWriteAll {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 1,
                        offset: s1_valid.len() as u64,
                        len: incoming_bytes.len(),
                    }
                })
                .expect("incoming write_all on seg 1");
            assert!(p_q0 < p_t0 && p_t0 < p_q1 && p_q1 < p_t1 && p_t1 < p_w1);

            assert_eq!(
                fs::read(init.paths.quarantine.join(format!("{old_q_id}.bin"))).unwrap(),
                old_bad
            );
            assert_eq!(
                fs::read(init.paths.quarantine.join(format!("{succ_q_id}.bin"))).unwrap(),
                succ_bad
            );
            assert_eq!(fs::metadata(&seg0_path).unwrap().len(), old_valid_end);
            assert_eq!(out.seg_id, 1);
            assert_eq!(out.frame_offset, s1_valid.len() as u64);
            assert_eq!(
                out.meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: old_valid_end,
                    sealed_crc32c: old_sealed_crc,
                }]
            );
            let s1_decoded = decode_all_segment_bytes(&fs::read(&seg1_path).unwrap());
            assert_eq!(s1_decoded.len(), 2);
            assert_eq!(s1_decoded[0].dedup_key, "rb08v-dbl-s1");
            assert_eq!(s1_decoded[1].dedup_key, "rb08v-dbl-new");
            assert_eq!(s1_decoded[1].payload, incoming_payload);
        }
    }

    /// `RB-08v` AC4–AC7: Nested Case C→D equality boundary where damaged tail
    /// immediately follows `committed_len` with ZERO complete uncommitted frames
    /// (`old_valid_end == committed_len < actual_len(S)`):
    /// - AC4: `quarantine/<id>.bin` full bytes equal exact `[committed_len, actual_len(S))`
    ///   with diagnostic code `LBR-AGENT-050`.
    /// - AC5: `events-<S>.log` is truncated back to exact original committed prefix
    ///   `[0..committed_len)`.
    /// - AC6: Dual `LTM2.sealed[]` seals exact `(S, committed_len, prefix_crc32c)`
    ///   and advances to `S + 1`.
    /// - AC7: Observer sequence proves `QuarantineFsyncDirComplete < SegmentTruncateStart`
    ///   on BOTH `recover_lane` and producer `append_frame` paths (plus AC8
    ///   `BeforeQuarantineRename` zero premature truncation and nested bad tail +
    ///   orphan `S + 1` convergence).
    #[test]
    fn nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal() {
        // =====================================================================
        // Part 1 — Equality boundary (`recover_lane` path, genuine truncated
        // half-frame immediately after `committed_len`): AC4, AC5, AC6, AC7, AC8
        // =====================================================================
        {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let p1_payload = b"rb08v-eq-p1".to_vec();
            let p2_payload = b"rb08v-eq-p2-committed-prefix".to_vec();
            let _out_p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08v-eq-p1",
                &p1_payload,
            )
            .unwrap();
            let out_p2 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08v-eq-p2",
                &p2_payload,
            )
            .unwrap();
            let committed_len = out_p2.meta.committed_len;
            let prefix_crc = out_p2.meta.prefix_crc32c;

            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            let expected_committed_prefix = fs::read(&seg0_path).unwrap();
            assert_eq!(expected_committed_prefix.len() as u64, committed_len);
            assert_eq!(crc32c_checksum(&expected_committed_prefix), prefix_crc);

            // Append ONLY a truncated half-frame immediately at `committed_len`
            // (zero complete uncommitted frames => `old_valid_end == committed_len`).
            let half_src = encode_frame(
                FRAME_FLAG_NONE,
                "rb08v-eq-half",
                b"half-frame-right-at-committed-boundary",
            )
            .unwrap();
            let bad_tail = half_src[..half_src.len() / 2].to_vec();
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&bad_tail).unwrap();
                f.sync_data().unwrap();
            }
            let pre_crash_seg0 = fs::read(&seg0_path).unwrap();

            let s1_payload = b"rb08v-eq-s1-valid".to_vec();
            let s1_bytes = encode_frame(FRAME_FLAG_NONE, "rb08v-eq-s1", &s1_payload).unwrap();
            write_bytes_0600(&seg1_path, &s1_bytes);

            let q_id =
                quarantine_artifact_id(CaptureQueueLane::Normal, 0, committed_len, &bad_tail);
            let q_bin = init.paths.quarantine.join(format!("{q_id}.bin"));

            // AC8 on equality boundary: crash before quarantine rename leaves seg 0 untouched.
            let mut obs_pre =
                SegmentObserver::with_crash(SegmentCrashPoint::BeforeQuarantineRename);
            let _ =
                recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs_pre)
                    .expect_err("crash before rename at equality boundary must fail");
            assert_eq!(fs::read(&seg0_path).unwrap(), pre_crash_seg0);
            assert!(!q_bin.exists());

            // Clean recovery: AC4, AC5, AC6, AC7.
            let mut obs = SegmentObserver::new();
            let rec = recover_lane_in_storage_dir(repo.path(), CaptureQueueLane::Normal, &mut obs)
                .expect("RB-08v equality boundary recovery must succeed");

            // AC4: Exact `[committed_len, actual_len(S))` bytes quarantined with `LBR-AGENT-050`.
            assert_eq!(rec.quarantined_artifacts.len(), 1);
            assert_eq!(rec.quarantined_artifacts[0].id, q_id);
            assert_eq!(
                rec.quarantined_artifacts[0].diagnostic_code,
                QUARANTINE_DIAGNOSTIC_CODE
            );
            assert_eq!(
                fs::read(&q_bin).expect("AC4: read quarantined bad tail"),
                bad_tail
            );

            // AC7: Observer proves `QuarantineFsyncDirComplete < SegmentTruncateStart(seg 0, committed_len)`.
            let pos_q_fsync = obs
                .events
                .iter()
                .position(|e| {
                    matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &q_id)
                })
                .expect("AC7: QuarantineFsyncDirComplete");
            let pos_trunc_start = obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: committed_len,
                    }
                })
                .expect("AC7: SegmentTruncateStart(0, committed_len)");
            let pos_seg_sync = obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentSyncData {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                    }
                })
                .expect("AC7: SegmentSyncData(0)");
            assert!(pos_q_fsync < pos_trunc_start && pos_trunc_start < pos_seg_sync);

            // AC5: `events-00000000.log` truncated back to exact original committed prefix.
            assert_eq!(
                fs::read(&seg0_path).unwrap(),
                expected_committed_prefix,
                "AC5 violated: events-00000000.log must equal exact [0..committed_len)"
            );

            // AC6: Dual `LTM2.sealed[]` seals `(0, committed_len, prefix_crc32c)`.
            let raw_meta = fs::read(tail_meta_path(&init.paths.normal)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(&init.paths.normal)).unwrap();
            assert_eq!(raw_meta, raw_bak);
            let disk_meta = LaneTailMeta::decode(&raw_meta).unwrap();
            assert_eq!(disk_meta, rec.meta);
            assert_eq!(
                disk_meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: committed_len,
                    sealed_crc32c: prefix_crc,
                }],
                "AC6 violated: LTM2.sealed[0] must record exact (0, committed_len, prefix_crc32c)"
            );
            assert_eq!(disk_meta.active_seg, 1);
            assert_eq!(disk_meta.committed_len, s1_bytes.len() as u64);
            assert_eq!(disk_meta.prefix_crc32c, crc32c_checksum(&s1_bytes));
            assert_eq!(fs::read(&seg1_path).unwrap(), s1_bytes);
        }

        // =====================================================================
        // Part 2 — Equality boundary via producer path with corrupt-header tail
        // immediately at `committed_len` + valid `S + 1`, plus nested damaged
        // old tail with empty orphan `S + 1` (`CaseDCleanOrphanSuccessor`).
        // =====================================================================
        {
            let repo = tempdir().unwrap();
            let init = init_capture_queue(repo.path()).unwrap();
            let p1 = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08v-eq-app-p1",
                b"eq-app-prefix",
            )
            .unwrap();
            let seg0_path = init.paths.normal.join(segment_log_file_name(0));
            let seg1_path = init.paths.normal.join(segment_log_file_name(1));
            let expected_prefix = fs::read(&seg0_path).unwrap();
            let corrupt_tail = b"BAD!\xde\xad\xbe\xefcorrupt-at-committed-boundary".to_vec();
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg0_path)
                    .unwrap();
                f.write_all(&corrupt_tail).unwrap();
                f.sync_data().unwrap();
            }
            let s1_valid = encode_frame(FRAME_FLAG_NONE, "rb08v-eq-app-s1", b"s1-body").unwrap();
            write_bytes_0600(&seg1_path, &s1_valid);

            let q_id = quarantine_artifact_id(
                CaptureQueueLane::Normal,
                0,
                p1.meta.committed_len,
                &corrupt_tail,
            );
            let new_payload = b"producer-append-after-eq-boundary".to_vec();
            let new_encoded =
                encode_frame(FRAME_FLAG_NONE, "rb08v-eq-app-new", &new_payload).unwrap();
            let mut app_obs = SegmentObserver::new();
            let out = append_validated_frame_in_storage_dir(
                repo.path(),
                CaptureQueueLane::Normal,
                &new_encoded,
                &mut app_obs,
            )
            .expect("producer path at equality boundary must succeed");

            let pos_q = app_obs
                .events
                .iter()
                .position(|e| {
                    matches!(e, SegmentOpEvent::QuarantineFsyncDirComplete { id } if id == &q_id)
                })
                .expect("AC7 producer equality boundary: QuarantineFsyncDirComplete");
            let pos_t = app_obs
                .events
                .iter()
                .position(|e| {
                    *e == SegmentOpEvent::SegmentTruncateStart {
                        lane: QUEUE_LANE_NORMAL_DIR,
                        seg_id: 0,
                        to_len: p1.meta.committed_len,
                    }
                })
                .expect("AC7 producer equality boundary: SegmentTruncateStart");
            assert!(pos_q < pos_t);
            assert_eq!(
                fs::read(init.paths.quarantine.join(format!("{q_id}.bin"))).unwrap(),
                corrupt_tail
            );
            assert_eq!(fs::read(&seg0_path).unwrap(), expected_prefix);
            assert_eq!(
                out.meta.sealed,
                vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: p1.meta.committed_len,
                    sealed_crc32c: p1.meta.prefix_crc32c,
                }]
            );
            assert_eq!(out.seg_id, 1);
            assert_eq!(out.frame_offset, s1_valid.len() as u64);

            // Bonus: nested damaged old tail on active `S = 1` + empty orphan `S = 2`
            // quarantines/truncates `S = 1`, unlinks empty `S = 2`, stays on `ACTIVE_SEG == 1`.
            let s1_clean_before = fs::read(&seg1_path).unwrap();
            let s1_bad = b"LCE1\x10\x00\x00\x00s1-bad-with-orphan".to_vec();
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg1_path)
                    .unwrap();
                f.write_all(&s1_bad).unwrap();
                f.sync_data().unwrap();
            }
            let seg2_path = init.paths.normal.join(segment_log_file_name(2));
            write_bytes_0600(&seg2_path, b"");
            let q_s1_id = quarantine_artifact_id(
                CaptureQueueLane::Normal,
                1,
                out.meta.committed_len,
                &s1_bad,
            );
            let rec_orp = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect("nested damaged old tail + empty orphan S+1 must converge cleanly");
            assert!(!seg2_path.exists());
            assert_eq!(rec_orp.meta.active_seg, 1);
            assert_eq!(rec_orp.meta.committed_len, out.meta.committed_len);
            assert_eq!(fs::read(&seg1_path).unwrap(), s1_clean_before);
            assert_eq!(
                fs::read(init.paths.quarantine.join(format!("{q_s1_id}.bin"))).unwrap(),
                s1_bad
            );
        }
    }

    /// `RB-08j` AC1–AC8: `MAX_SEALED_SEGMENTS = 256` normal 16 MiB rotation
    /// pre-mutation backpressure (`LBR-AGENT-046`) and `reclaim_sealed_segment`
    /// slot release recovery:
    /// - AC1: 256 × 4 KiB small sealed segments + near-16 MiB active segment
    ///   (sibling lane 0B, quarantine 0B) satisfies
    ///   `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() <= 256 MiB`
    ///   (strictly isolating Step ② (b) from Step ② (a) global byte cap).
    /// - AC2: Normal 16 MiB rotation on clean Case A full table (`256 -> 257`)
    ///   returns recoverable backpressure `StorageBackendError::QueueFull`
    ///   (`LBR-AGENT-046`).
    /// - AC3–AC6: Strictly zero disk mutation on rejection — no `events-00000257.log`,
    ///   byte-for-byte unchanged `events-00000256.log`, `tail.meta`, `tail.meta.bak`.
    /// - AC7: `reclaim_sealed_segment` atomically commits `LTM2` (`MIN_LIVE_SEG = 1`,
    ///   `sealed.len() = 255`) strictly BEFORE `unlink(events-00000000.log)` +
    ///   `fsync_dir(lane_dir)` (also verifies non-oldest/active rejection and
    ///   crash-before-unlink idempotence); retrying the same rotation frame now
    ///   succeeds and creates `events-00000257.log` (`0600`).
    /// - AC8: Dual `LTM2` records `(256, old_committed_len, old_prefix_crc32c)` in
    ///   `LTM2.sealed[]`, advances `ACTIVE_SEG = 257`, and new frame decodes
    ///   100% losslessly from `events-00000257.log`.
    #[test]
    fn sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release() {
        assert_eq!(MAX_SEALED_SEGMENTS, 256);

        let repo = tempdir().expect("tempdir");
        let init = init_capture_queue(repo.path()).expect("init capture queue");
        let normal_dir = &init.paths.normal;

        // 1) Construct 256 × ~4 KiB small sealed segments (`seg 0..256`, ~1 MiB total).
        let small_payload = vec![0x42u8; 4096];
        let small_frame =
            encode_frame(FRAME_FLAG_NONE, "rb08j-sealed-4k", &small_payload).expect("encode 4k");
        let small_len = small_frame.len() as u64;
        let small_crc = crc32c_checksum(&small_frame);
        let mut sealed = Vec::with_capacity(MAX_SEALED_SEGMENTS);
        for seg_id in 0..MAX_SEALED_SEGMENTS as u32 {
            let p = normal_dir.join(segment_log_file_name(seg_id));
            write_bytes_0600(&p, &small_frame);
            sealed.push(SealedSegmentMeta {
                seg_id,
                sealed_len: small_len,
                sealed_crc32c: small_crc,
            });
        }

        // 2) Construct clean Case A active segment `seg 256` near 16 MiB
        //    (`16 MiB - 1024` bytes) composed of valid LCE1 frames.
        let active_seg_id: u32 = MAX_SEALED_SEGMENTS as u32; // 256
        let chunk_payload = vec![0x61u8; 2 * 1024 * 1024]; // 2 MiB payload per frame
        let chunk_frame =
            encode_frame(FRAME_FLAG_NONE, "rb08j-active-2m", &chunk_payload).expect("encode 2m");
        let target_active_len = CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES - 1024;
        let mut active_bytes = Vec::with_capacity(target_active_len as usize);
        while active_bytes.len() + chunk_frame.len() <= target_active_len as usize {
            active_bytes.extend_from_slice(&chunk_frame);
        }
        // Pad with one exact-sized valid frame so `active_bytes.len() == target_active_len`.
        let remaining = target_active_len as usize - active_bytes.len();
        let pad_key = "rb08j-active-pad";
        let frame_overhead = crate::storage::queue::FRAME_HEADER_LEN
            + crate::storage::queue::FRAME_BODY_FIXED_OVERHEAD
            + pad_key.len();
        assert!(remaining > frame_overhead);
        let pad_payload = vec![0x62u8; remaining - frame_overhead];
        let pad_frame = encode_frame(FRAME_FLAG_NONE, pad_key, &pad_payload).expect("encode pad");
        active_bytes.extend_from_slice(&pad_frame);
        assert_eq!(active_bytes.len() as u64, target_active_len);

        let seg256_path = normal_dir.join(segment_log_file_name(active_seg_id));
        let seg257_path = normal_dir.join(segment_log_file_name(active_seg_id + 1));
        write_bytes_0600(&seg256_path, &active_bytes);

        let old_committed_len = active_bytes.len() as u64;
        let old_prefix_crc = crc32c_checksum(&active_bytes);
        let full_meta = LaneTailMeta {
            min_live_seg: 0,
            active_seg: active_seg_id,
            committed_len: old_committed_len,
            prefix_crc32c: old_prefix_crc,
            sealed,
        };
        let encoded_meta = full_meta.encode().expect("encode 256-sealed LTM2");
        write_bytes_0600(&tail_meta_path(normal_dir), &encoded_meta);
        write_bytes_0600(&tail_meta_bak_path(normal_dir), &encoded_meta);

        // Incoming frame (~2 KiB) crosses 16 MiB on seg 256 (`16 MiB - 1024 + ~2048 > 16 MiB`).
        let rot_payload = vec![0x77u8; 2048];
        let rot_frame =
            encode_frame(FRAME_FLAG_NONE, "rb08j-rotate-257", &rot_payload).expect("encode rot");
        assert!(old_committed_len + rot_frame.len() as u64 > CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES);

        // AC1: Total combined occupancy including incoming frame is far <= 256 MiB
        // (~17 MiB << 256 MiB), strictly isolating Step ② (b) from Step ② (a).
        let occ = measure_capture_queue_occupancy(repo.path()).expect("measure occupancy");
        assert_eq!(occ.priority_bytes, 0);
        assert_eq!(occ.quarantine_bytes, 0);
        assert!(
            occ.total_bytes + rot_frame.len() as u64 <= CAPTURE_QUEUE_MAX_TOTAL_BYTES,
            "AC1 violated: fixture total ({}) + incoming ({}) must be <= 256 MiB",
            occ.total_bytes,
            rot_frame.len()
        );

        let seg256_bytes_before = fs::read(&seg256_path).unwrap();
        let meta_bytes_before = fs::read(tail_meta_path(normal_dir)).unwrap();
        let bak_bytes_before = fs::read(tail_meta_bak_path(normal_dir)).unwrap();

        // AC2: Clean Case A normal rotation `256 -> 257` fails closed with `LBR-AGENT-046`.
        let err = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &rot_frame)
            .expect_err("AC2: normal rotation at 256/256 sealed table must return LBR-AGENT-046");
        assert!(
            matches!(err, StorageBackendError::QueueFull { .. }),
            "AC2 violated: expected QueueFull, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-AGENT-046");

        // AC3: Zero `events-00000257.log` created on disk.
        assert!(
            !seg257_path.exists(),
            "AC3 violated: rejected rotation must not create events-00000257.log"
        );
        // AC4: Existing `events-00000256.log` byte-for-byte unchanged.
        assert_eq!(
            fs::read(&seg256_path).unwrap(),
            seg256_bytes_before,
            "AC4 violated: rejected rotation must not modify events-00000256.log"
        );
        // AC5: Primary `tail.meta` byte-for-byte unchanged.
        assert_eq!(
            fs::read(tail_meta_path(normal_dir)).unwrap(),
            meta_bytes_before,
            "AC5 violated: rejected rotation must not modify tail.meta"
        );
        // AC6: Backup `tail.meta.bak` byte-for-byte unchanged.
        assert_eq!(
            fs::read(tail_meta_bak_path(normal_dir)).unwrap(),
            bak_bytes_before,
            "AC6 violated: rejected rotation must not modify tail.meta.bak"
        );

        // Guard checks on `reclaim_sealed_segment`:
        // 1) Reject unconsumed (`seg_id >= highwater_seg`), non-oldest (`seg 1`),
        //    and active (`seg 256`) segments with zero disk mutation.
        assert!(
            reclaim_sealed_segment(repo.path(), CaptureQueueLane::Normal, 0, 0).is_err(),
            "reclaim must reject unconsumed segment at or beyond consumer highwater"
        );
        assert!(
            reclaim_sealed_segment(repo.path(), CaptureQueueLane::Normal, 1, 2).is_err(),
            "reclaim must reject non-oldest sealed segment"
        );
        assert!(
            reclaim_sealed_segment(
                repo.path(),
                CaptureQueueLane::Normal,
                active_seg_id,
                active_seg_id + 1,
            )
            .is_err(),
            "reclaim must reject active segment"
        );
        assert_eq!(
            fs::read(tail_meta_path(normal_dir)).unwrap(),
            meta_bytes_before
        );
        assert_eq!(
            fs::read(tail_meta_bak_path(normal_dir)).unwrap(),
            bak_bytes_before
        );

        // 2) Pre-Phase-2 target validation with divergent replicas (`tail.meta.bak`
        //    lagging behind `tail.meta`, plus planted `tail.meta.tmp` and
        //    `quarantine/normal-stale.tmp`): missing, symlink, and foreign-owned
        //    targets MUST fail closed strictly BEFORE any Phase 2 mirror repair
        //    or `.tmp` cleanup (exact byte-for-byte zero mutation).
        let seg0_path = normal_dir.join(segment_log_file_name(0));
        let seg1_path = normal_dir.join(segment_log_file_name(1));
        {
            let lagging_bak = LaneTailMeta {
                min_live_seg: 0,
                active_seg: active_seg_id,
                committed_len: 0,
                prefix_crc32c: 0,
                sealed: full_meta.sealed.clone(),
            };
            let lagging_bak_bytes = lagging_bak.encode().expect("encode lagging bak");
            write_bytes_0600(&tail_meta_bak_path(normal_dir), &lagging_bak_bytes);
            let lane_tmp_path = normal_dir.join(QUEUE_TAIL_META_TMP_FILE);
            let q_tmp_path = init.paths.quarantine.join("normal-seg0-off0-stale.tmp");
            write_bytes_0600(&lane_tmp_path, b"stale-lane-tmp");
            write_bytes_0600(&q_tmp_path, b"stale-quarantine-tmp");

            let assert_divergent_zero_mutation = |case: &str| {
                assert_eq!(
                    fs::read(tail_meta_path(normal_dir)).unwrap(),
                    meta_bytes_before,
                    "{case}: primary tail.meta must remain unchanged"
                );
                assert_eq!(
                    fs::read(tail_meta_bak_path(normal_dir)).unwrap(),
                    lagging_bak_bytes,
                    "{case}: lagging tail.meta.bak must NOT be repaired on rejected reclaim"
                );
                assert_eq!(
                    fs::read(&lane_tmp_path).unwrap(),
                    b"stale-lane-tmp",
                    "{case}: lane .tmp must NOT be cleaned on rejected reclaim"
                );
                assert_eq!(
                    fs::read(&q_tmp_path).unwrap(),
                    b"stale-quarantine-tmp",
                    "{case}: quarantine .tmp must NOT be cleaned on rejected reclaim"
                );
            };

            // 2a) Unconsumed segment with divergent replicas -> zero mutation.
            let unconsumed_err =
                reclaim_sealed_segment(repo.path(), CaptureQueueLane::Normal, 0, 0)
                    .expect_err("unconsumed seg with divergent replicas must fail");
            assert!(matches!(
                unconsumed_err,
                StorageBackendError::BackendInternal { .. }
            ));
            assert_divergent_zero_mutation("unconsumed-divergent");

            // 2b) Foreign-owned target (cross-platform: Unix UID + Windows SID)
            //     with divergent replicas -> owner mismatch + zero mutation.
            let mut foreign_obs = SegmentObserver::with_foreign_reclaim_owner();
            let foreign_err = reclaim_sealed_segment_in_storage_dir(
                repo.path(),
                CaptureQueueLane::Normal,
                0,
                1,
                &mut foreign_obs,
            )
            .expect_err("foreign-owned target must be rejected before Phase 2");
            assert!(
                crate::storage::queue::is_capture_queue_owner_mismatch_error(&foreign_err),
                "expected owner mismatch error, got {foreign_err:?}"
            );
            assert_divergent_zero_mutation("foreign-owner-divergent");

            // 2c) Missing live sealed segment (`events-00000000.log` deleted while
            //     still listed in `LTM2.sealed[0]`) -> `CorruptCommittedFrame`
            //     (`LBR-IO-001`) + zero mutation (NEVER silently retire missing seg!).
            let saved_seg0 = fs::read(&seg0_path).unwrap();
            fs::remove_file(&seg0_path).unwrap();
            let missing_err = reclaim_sealed_segment(repo.path(), CaptureQueueLane::Normal, 0, 1)
                .expect_err("missing live sealed segment must fail closed before Phase 2");
            assert!(
                matches!(
                    missing_err,
                    StorageBackendError::CorruptCommittedFrame { .. }
                ),
                "expected CorruptCommittedFrame on missing sealed seg, got {missing_err:?}"
            );
            assert_eq!(missing_err.stable_code_str(), "LBR-IO-001");
            assert_divergent_zero_mutation("missing-target-divergent");

            // 2d) Symlink target (`events-00000000.log` -> decoy) with divergent
            //     replicas -> symlink error + zero mutation.
            #[cfg(unix)]
            {
                let decoy_path = repo.path().join("decoy-seg0.log");
                write_bytes_0600(&decoy_path, &saved_seg0);
                std::os::unix::fs::symlink(&decoy_path, &seg0_path).unwrap();
                let sym_err = reclaim_sealed_segment(repo.path(), CaptureQueueLane::Normal, 0, 1)
                    .expect_err("symlink sealed target must fail closed before Phase 2");
                assert!(
                    crate::storage::queue::is_capture_queue_symlink_rejected_error(&sym_err),
                    "expected symlink rejection, got {sym_err:?}"
                );
                assert_divergent_zero_mutation("symlink-target-divergent");
                fs::remove_file(&seg0_path).unwrap();
                fs::remove_file(&decoy_path).unwrap();
            }

            // Restore clean `events-00000000.log`, synced `tail.meta.bak`, and clean `.tmp`.
            write_bytes_0600(&seg0_path, &saved_seg0);
            write_bytes_0600(&tail_meta_bak_path(normal_dir), &bak_bytes_before);
            fs::remove_file(&lane_tmp_path).unwrap();
            fs::remove_file(&q_tmp_path).unwrap();
        }

        // Verify `ADR-RB-05` #4 ③ crash boundary: crash after dual `LTM2` commit
        // and BEFORE `unlink(seg 0)` leaves `seg 0` as reclaimed orphan (`< min_live_seg`),
        // cleanly unlinked on subsequent `recover_lane`, AND verify normal reclaim
        // ordering (`dual LTM2 < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim`) on seg 1!
        let mut crash_reclaim =
            SegmentObserver::with_crash(SegmentCrashPoint::AfterReclaimMetaBeforeUnlink);
        let _ = reclaim_sealed_segment_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            0,
            1,
            &mut crash_reclaim,
        )
        .expect_err("simulated crash before unlink must fail");
        assert!(
            seg0_path.exists(),
            "seg 0 still on disk right after pre-unlink crash"
        );
        let meta_after_crash = read_converged_lane_tail_meta(normal_dir).unwrap();
        assert_eq!(meta_after_crash.min_live_seg, 1);
        assert_eq!(meta_after_crash.sealed.len(), 255);
        let rec_orphan = recover_lane(repo.path(), CaptureQueueLane::Normal)
            .expect("recover_lane must clean reclaimed orphan seg 0");
        assert!(
            !seg0_path.exists(),
            "reclaimed orphan seg 0 cleaned by recovery"
        );
        assert_eq!(rec_orphan.reclaimed_orphans_removed, 1);

        // Normal `reclaim_sealed_segment` on seg 1 verifies strict `LTM2 < unlink < fsync_dir`.
        let mut reclaim_obs = SegmentObserver::new();
        let reclaim_out = reclaim_sealed_segment_in_storage_dir(
            repo.path(),
            CaptureQueueLane::Normal,
            1,
            2,
            &mut reclaim_obs,
        )
        .expect("reclaim_sealed_segment(1, 2) must succeed");
        assert_eq!(reclaim_out.reclaimed_seg_id, 1);
        assert!(reclaim_out.file_removed);
        assert!(!seg1_path.exists());
        assert_eq!(reclaim_out.meta.min_live_seg, 2);
        assert_eq!(reclaim_out.meta.sealed.len(), 254);
        let pos_meta_bak = reclaim_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterMeta {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    target_name: QUEUE_TAIL_META_BAK_FILE,
                }
            })
            .expect("reclaim dual LTM2 commit");
        let pos_unlink = reclaim_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::ReclaimSegmentUnlink {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("reclaim unlink(1)");
        let pos_fsync = reclaim_obs
            .events
            .iter()
            .position(|e| {
                *e == SegmentOpEvent::FsyncLaneDirAfterReclaim {
                    lane: QUEUE_LANE_NORMAL_DIR,
                    seg_id: 1,
                }
            })
            .expect("reclaim fsync_dir after unlink(1)");
        assert!(
            pos_meta_bak < pos_unlink && pos_unlink < pos_fsync,
            "ADR-RB-05 #4 ③ violated: dual LTM2 must commit strictly before unlink and fsync_dir"
        );

        // AC7: After freeing slots (`sealed.len() < 256`), retrying the exact same
        // rotation-triggering frame succeeds and creates `events-00000257.log` (`0600`).
        let rot_out = append_encoded_frame(repo.path(), CaptureQueueLane::Normal, &rot_frame)
            .expect("AC7: rotation must recover and succeed after reclaim_sealed_segment");
        assert_eq!(rot_out.seg_id, active_seg_id + 1);
        assert_eq!(rot_out.frame_offset, 0);
        assert!(
            seg257_path.exists(),
            "AC7 violated: events-00000257.log must be created after slot release"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&seg257_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // AC8: Dual `LTM2` seals `(256, old_committed_len, old_prefix_crc)` into
        // `LTM2.sealed[]`, advances `ACTIVE_SEG = 257`, and `events-00000257.log`
        // decodes `rot_frame` 100% losslessly.
        let raw_meta = fs::read(tail_meta_path(normal_dir)).unwrap();
        let raw_bak = fs::read(tail_meta_bak_path(normal_dir)).unwrap();
        assert_eq!(raw_meta, raw_bak);
        let final_meta = LaneTailMeta::decode(&raw_meta).unwrap();
        assert_eq!(final_meta, rot_out.meta);
        assert_eq!(final_meta.min_live_seg, 2);
        assert_eq!(final_meta.active_seg, active_seg_id + 1);
        assert_eq!(final_meta.sealed.len(), 255);
        assert_eq!(
            final_meta.sealed.last(),
            Some(&SealedSegmentMeta {
                seg_id: active_seg_id,
                sealed_len: old_committed_len,
                sealed_crc32c: old_prefix_crc,
            }),
            "AC8 violated: LTM2.sealed[] must record (256, old_committed_len, old_prefix_crc)"
        );
        assert_eq!(final_meta.committed_len, rot_frame.len() as u64);
        assert_eq!(final_meta.prefix_crc32c, crc32c_checksum(&rot_frame));
        let seg257_decoded = decode_all_segment_bytes(&fs::read(&seg257_path).unwrap());
        assert_eq!(seg257_decoded.len(), 1);
        assert_eq!(seg257_decoded[0].dedup_key, "rb08j-rotate-257");
        assert_eq!(seg257_decoded[0].payload, rot_payload);
    }

    /// `RB-08r` AC1–AC6: Case D full 256-entry `LTM2.sealed[]` valid successor
    /// fail-closed rejection (including nested damaged old-segment tail +
    /// planted `.tmp` whole-disk zero mutation) and `LTM2` decoder structural
    /// upper-bound guards (`SEALED_COUNT > 256` and
    /// `SEALED_COUNT != ACTIVE_SEG - MIN_LIVE_SEG`).
    #[test]
    fn sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected() {
        use crate::storage::queue::{LTM2_MAGIC, LTM2_VERSION, Ltm2CodecError};

        fn build_raw_ltm2_with_valid_crc(
            min_live_seg: u32,
            active_seg: u32,
            committed_len: u64,
            prefix_crc32c: u32,
            declared_sealed_count: u32,
            entries: &[SealedSegmentMeta],
        ) -> Vec<u8> {
            let mut buf = Vec::with_capacity(
                crate::storage::queue::LTM2_MIN_LEN
                    + entries.len() * crate::storage::queue::LTM2_SEALED_ENTRY_LEN,
            );
            buf.extend_from_slice(&LTM2_MAGIC);
            buf.extend_from_slice(&LTM2_VERSION.to_le_bytes());
            buf.extend_from_slice(&min_live_seg.to_le_bytes());
            buf.extend_from_slice(&active_seg.to_le_bytes());
            buf.extend_from_slice(&committed_len.to_le_bytes());
            buf.extend_from_slice(&prefix_crc32c.to_le_bytes());
            buf.extend_from_slice(&declared_sealed_count.to_le_bytes());
            for entry in entries {
                buf.extend_from_slice(&entry.seg_id.to_le_bytes());
                buf.extend_from_slice(&entry.sealed_len.to_le_bytes());
                buf.extend_from_slice(&entry.sealed_crc32c.to_le_bytes());
            }
            let crc = crc32c_checksum(&buf);
            buf.extend_from_slice(&crc.to_le_bytes());
            buf
        }

        // =====================================================================
        // Part 1 — AC3 & AC4: `LTM2` decoder structural bounds (with valid CRC)
        // =====================================================================
        {
            // AC3a: Genuine 257-entry wire buffer (4146B) with valid META_CRC32C.
            let entries_257: Vec<SealedSegmentMeta> = (0..257u32)
                .map(|seg_id| SealedSegmentMeta {
                    seg_id,
                    sealed_len: 128,
                    sealed_crc32c: seg_id.wrapping_mul(31).wrapping_add(7),
                })
                .collect();
            let raw_257_full =
                build_raw_ltm2_with_valid_crc(0, 257, 64, 0x1122_3344, 257, &entries_257);
            assert_eq!(raw_257_full.len(), 34 + 16 * 257);
            let err_257_full = LaneTailMeta::decode(&raw_257_full)
                .expect_err("AC3: 257-entry LTM2 buffer must be rejected");
            assert!(
                matches!(err_257_full, Ltm2CodecError::InvalidInvariant { .. }),
                "AC3: expected InvalidInvariant for 257-entry LTM2, got {err_257_full:?}"
            );
            assert_eq!(
                err_257_full.into_storage_error().stable_code_str(),
                "LBR-IO-001"
            );

            // AC3b: Compact 34-byte header declaring SEALED_COUNT = 257 with valid CRC.
            let raw_257_hdr = build_raw_ltm2_with_valid_crc(0, 257, 0, 0, 257, &[]);
            let err_257_hdr = LaneTailMeta::decode(&raw_257_hdr)
                .expect_err("AC3: header SEALED_COUNT=257 must be rejected");
            assert!(matches!(
                err_257_hdr,
                Ltm2CodecError::InvalidInvariant { .. }
            ));

            // AC3c: SEALED_COUNT = u32::MAX with valid CRC.
            let raw_max_u32 = build_raw_ltm2_with_valid_crc(0, 0, 0, 0, u32::MAX, &[]);
            let err_max_u32 = LaneTailMeta::decode(&raw_max_u32)
                .expect_err("AC3: SEALED_COUNT=u32::MAX must be rejected");
            assert!(matches!(
                err_max_u32,
                Ltm2CodecError::InvalidInvariant { .. }
            ));

            // AC4a: SEALED_COUNT < ACTIVE_SEG - MIN_LIVE_SEG (2 entries vs 3 expected).
            let entries_2 = vec![
                SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: 100,
                    sealed_crc32c: 1,
                },
                SealedSegmentMeta {
                    seg_id: 1,
                    sealed_len: 200,
                    sealed_crc32c: 2,
                },
            ];
            let raw_too_few = build_raw_ltm2_with_valid_crc(0, 3, 50, 0xabcd, 2, &entries_2);
            let err_too_few = LaneTailMeta::decode(&raw_too_few)
                .expect_err("AC4: SEALED_COUNT < ACTIVE_SEG - MIN_LIVE_SEG must be rejected");
            assert!(matches!(
                err_too_few,
                Ltm2CodecError::InvalidInvariant { .. }
            ));
            assert_eq!(
                err_too_few.into_storage_error().stable_code_str(),
                "LBR-IO-001"
            );

            // AC4b: SEALED_COUNT > ACTIVE_SEG - MIN_LIVE_SEG (2 entries vs 1 expected).
            let raw_too_many = build_raw_ltm2_with_valid_crc(0, 1, 50, 0xabcd, 2, &entries_2);
            let err_too_many = LaneTailMeta::decode(&raw_too_many)
                .expect_err("AC4: SEALED_COUNT > ACTIVE_SEG - MIN_LIVE_SEG must be rejected");
            assert!(matches!(
                err_too_many,
                Ltm2CodecError::InvalidInvariant { .. }
            ));

            // AC4c: MIN_LIVE_SEG > ACTIVE_SEG with SEALED_COUNT = 0.
            let raw_inverted = build_raw_ltm2_with_valid_crc(5, 4, 0, 0, 0, &[]);
            let err_inverted = LaneTailMeta::decode(&raw_inverted)
                .expect_err("AC4: MIN_LIVE_SEG > ACTIVE_SEG must be rejected");
            assert!(matches!(
                err_inverted,
                Ltm2CodecError::InvalidInvariant { .. }
            ));

            // AC4d: ACTIVE_SEG - MIN_LIVE_SEG = 257 while SEALED_COUNT = 256.
            let raw_span_257 =
                build_raw_ltm2_with_valid_crc(0, 257, 64, 0x1122_3344, 256, &entries_257[..256]);
            let err_span_257 = LaneTailMeta::decode(&raw_span_257)
                .expect_err("AC4: active-min_live=257 with sealed_count=256 must be rejected");
            assert!(matches!(
                err_span_257,
                Ltm2CodecError::InvalidInvariant { .. }
            ));

            // AC4e: Non-contiguous seg_id inside sealed[] even when count matches span.
            let non_contig = vec![
                SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: 100,
                    sealed_crc32c: 1,
                },
                SealedSegmentMeta {
                    seg_id: 2,
                    sealed_len: 200,
                    sealed_crc32c: 2,
                },
            ];
            let raw_non_contig = build_raw_ltm2_with_valid_crc(0, 2, 0, 0, 2, &non_contig);
            assert!(matches!(
                LaneTailMeta::decode(&raw_non_contig),
                Err(Ltm2CodecError::InvalidInvariant { .. })
            ));

            // Disk-level proof that corrupt 257-entry & count-mismatch replicas
            // cause lane recovery/append to fail closed (`LBR-IO-001`) with zero mutation.
            for (label, corrupt_meta_bytes) in [
                ("disk-257-entries", &raw_257_full),
                ("disk-count-mismatch", &raw_too_few),
            ] {
                let repo = tempdir().expect("tempdir");
                let init = init_capture_queue(repo.path()).expect("init capture queue");
                let frame = encode_frame(FRAME_FLAG_NONE, "rb08r-disk-meta", b"seed").unwrap();
                write_bytes_0600(&init.paths.normal.join(segment_log_file_name(0)), &frame);
                write_bytes_0600(&tail_meta_path(&init.paths.normal), corrupt_meta_bytes);
                write_bytes_0600(&tail_meta_bak_path(&init.paths.normal), corrupt_meta_bytes);
                let lane_before = snapshot_tree(&init.paths.normal);
                let q_before = snapshot_tree(&init.paths.quarantine);

                let err = recover_lane(repo.path(), CaptureQueueLane::Normal)
                    .expect_err("disk-level invalid LTM2 must fail closed");
                assert!(
                    matches!(err, StorageBackendError::CorruptCommittedFrame { .. }),
                    "{label}: expected CorruptCommittedFrame, got {err:?}"
                );
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert_eq!(snapshot_tree(&init.paths.normal), lane_before);
                assert_eq!(snapshot_tree(&init.paths.quarantine), q_before);
            }
        }

        // Helper: populate a 256-sealed lane (`seg 0..256` sealed + clean `seg 256` active).
        let build_full_256_lane = || {
            let repo = tempdir().expect("tempdir");
            let init = init_capture_queue(repo.path()).expect("init capture queue");
            let normal_dir = init.paths.normal.clone();

            let small_frame =
                encode_frame(FRAME_FLAG_NONE, "rb08r-sealed-seg", &[0x55u8; 512]).unwrap();
            let small_len = small_frame.len() as u64;
            let small_crc = crc32c_checksum(&small_frame);
            let mut sealed = Vec::with_capacity(MAX_SEALED_SEGMENTS);
            for seg_id in 0..MAX_SEALED_SEGMENTS as u32 {
                write_bytes_0600(
                    &normal_dir.join(segment_log_file_name(seg_id)),
                    &small_frame,
                );
                sealed.push(SealedSegmentMeta {
                    seg_id,
                    sealed_len: small_len,
                    sealed_crc32c: small_crc,
                });
            }

            let active_seg = MAX_SEALED_SEGMENTS as u32; // 256
            let active_frame =
                encode_frame(FRAME_FLAG_NONE, "rb08r-active-256", b"active-256-committed").unwrap();
            write_bytes_0600(
                &normal_dir.join(segment_log_file_name(active_seg)),
                &active_frame,
            );
            let full_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg,
                committed_len: active_frame.len() as u64,
                prefix_crc32c: crc32c_checksum(&active_frame),
                sealed,
            };
            let encoded_meta = full_meta.encode().expect("encode 256-entry LTM2");
            write_bytes_0600(&tail_meta_path(&normal_dir), &encoded_meta);
            write_bytes_0600(&tail_meta_bak_path(&normal_dir), &encoded_meta);
            (repo, init, full_meta, encoded_meta)
        };

        // =====================================================================
        // Part 2 — AC1 & AC2: Clean full 256-entry table + valid successor S+1
        // =====================================================================
        {
            let (repo, init, _full_meta, encoded_meta) = build_full_256_lane();
            let normal_dir = &init.paths.normal;
            let succ_frame = encode_frame(
                FRAME_FLAG_NONE,
                "rb08r-illegal-s257",
                b"valid-successor-frame",
            )
            .unwrap();
            write_bytes_0600(&normal_dir.join(segment_log_file_name(257)), &succ_frame);

            let lane_before = snapshot_tree(normal_dir);
            let q_before = snapshot_tree(&init.paths.quarantine);

            // Both `recover_lane` and producer `append_frame` reject with LBR-IO-001.
            let rec_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect_err("AC1: Case D full 256 table + valid S+1 must fail closed");
            assert!(
                matches!(rec_err, StorageBackendError::CorruptCommittedFrame { .. }),
                "AC1 violated: expected CorruptCommittedFrame, got {rec_err:?}"
            );
            assert_eq!(rec_err.stable_code_str(), "LBR-IO-001");

            let app_err = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08r-new-incoming",
                b"must-not-write",
            )
            .expect_err("AC1 (append_frame): Case D full 256 table + valid S+1 must fail closed");
            assert!(matches!(
                app_err,
                StorageBackendError::CorruptCommittedFrame { .. }
            ));
            assert_eq!(app_err.stable_code_str(), "LBR-IO-001");

            // AC2: Disk `LTM2.sealed.len()` remains strictly 256 (zero 257th entry).
            let raw_meta = fs::read(tail_meta_path(normal_dir)).unwrap();
            let raw_bak = fs::read(tail_meta_bak_path(normal_dir)).unwrap();
            assert_eq!(raw_meta, encoded_meta);
            assert_eq!(raw_bak, encoded_meta);
            assert_eq!(LaneTailMeta::decode(&raw_meta).unwrap().sealed.len(), 256);
            assert_eq!(LaneTailMeta::decode(&raw_bak).unwrap().sealed.len(), 256);
            assert_eq!(snapshot_tree(normal_dir), lane_before);
            assert_eq!(snapshot_tree(&init.paths.quarantine), q_before);
        }

        // =====================================================================
        // Part 3 — AC5 & AC6: Full 256 table + nested damaged old-segment tail
        // (`actual_len(S) > committed_len`) + valid successor `S + 1` + planted
        // `tail.meta.tmp`, `tail.meta.bak.tmp`, `quarantine/<lane>-*.tmp`
        // (and lagging backup replica!) -> Phase 1 read-only `LBR-IO-001` &
        // exact whole-directory zero mutation.
        // =====================================================================
        for (case_label, include_synced_tail, succ_has_half_tail, lag_backup_replica) in [
            ("nested-synced-plus-bad-tail-clean-s257", true, false, false),
            (
                "nested-bad-at-committed-plus-bad-s257-and-lag-bak",
                false,
                true,
                true,
            ),
        ] {
            let (repo, init, full_meta, _encoded_meta) = build_full_256_lane();
            let normal_dir = &init.paths.normal;
            let seg256_path = normal_dir.join(segment_log_file_name(256));
            let seg257_path = normal_dir.join(segment_log_file_name(257));

            // Append uncommitted tail to old active segment `seg 256`.
            let extra_valid =
                encode_frame(FRAME_FLAG_NONE, "rb08r-nested-synced", b"synced-tail-frame").unwrap();
            let half_src = encode_frame(
                FRAME_FLAG_NONE,
                "rb08r-nested-half",
                b"truncated-old-tail-bytes",
            )
            .unwrap();
            let half_bytes = &half_src[..half_src.len() / 2];
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&seg256_path)
                    .unwrap();
                if include_synced_tail {
                    f.write_all(&extra_valid).unwrap();
                }
                f.write_all(half_bytes).unwrap();
                f.sync_data().unwrap();
            }
            assert!(fs::metadata(&seg256_path).unwrap().len() > full_meta.committed_len);

            // Create valid successor `events-00000257.log` (optionally with trailing half-frame).
            let mut succ_bytes =
                encode_frame(FRAME_FLAG_NONE, "rb08r-nested-s257", b"valid-s257-payload").unwrap();
            if succ_has_half_tail {
                let succ_half =
                    encode_frame(FRAME_FLAG_NONE, "rb08r-s257-half", b"s257-trailing-half")
                        .unwrap();
                succ_bytes.extend_from_slice(&succ_half[..succ_half.len() / 2]);
            }
            write_bytes_0600(&seg257_path, &succ_bytes);

            // Optionally diverge `tail.meta.bak` to prove Phase 1 rejects before
            // ANY Subcase B1 / mirror repair runs!
            if lag_backup_replica {
                let lagging_bak = LaneTailMeta {
                    min_live_seg: 0,
                    active_seg: 256,
                    committed_len: 0,
                    prefix_crc32c: 0,
                    sealed: full_meta.sealed.clone(),
                };
                write_bytes_0600(
                    &tail_meta_bak_path(normal_dir),
                    &lagging_bak.encode().unwrap(),
                );
            }

            // Plant lane `.tmp` files and shared `quarantine/` `.tmp` files.
            write_bytes_0600(
                &normal_dir.join(QUEUE_TAIL_META_TMP_FILE),
                b"planted-meta-tmp",
            );
            write_bytes_0600(
                &normal_dir.join(QUEUE_TAIL_META_BAK_TMP_FILE),
                b"planted-meta-bak-tmp",
            );
            write_bytes_0600(
                &init
                    .paths
                    .quarantine
                    .join("normal-seg256-off0-deadbeef.tmp"),
                b"planted-normal-quarantine-tmp",
            );
            write_bytes_0600(
                &init
                    .paths
                    .quarantine
                    .join("priority-seg0-off0-cafebabe.tmp"),
                b"planted-priority-quarantine-tmp",
            );

            let full_lane_snapshot_before = snapshot_tree(normal_dir);
            let full_quarantine_snapshot_before = snapshot_tree(&init.paths.quarantine);

            // AC5: Phase 1 read-only scan immediately returns `CorruptCommittedFrame` (`LBR-IO-001`).
            let rec_err = recover_lane(repo.path(), CaptureQueueLane::Normal)
                .expect_err("AC5: full 256 + nested damaged old tail + valid S+1 must fail closed");
            assert!(
                matches!(rec_err, StorageBackendError::CorruptCommittedFrame { .. }),
                "{case_label}: expected CorruptCommittedFrame from recover_lane, got {rec_err:?}"
            );
            assert_eq!(rec_err.stable_code_str(), "LBR-IO-001");

            // AC6: Exact full-disk snapshot equality across both `normal/` and `quarantine/`.
            assert_eq!(
                snapshot_tree(normal_dir),
                full_lane_snapshot_before,
                "{case_label}: AC6 violated after recover_lane — normal/ mutated"
            );
            assert_eq!(
                snapshot_tree(&init.paths.quarantine),
                full_quarantine_snapshot_before,
                "{case_label}: AC6 violated after recover_lane — quarantine/ mutated"
            );

            // Same check on producer `append_frame` entry point.
            let app_err = append_frame(
                repo.path(),
                CaptureQueueLane::Normal,
                FRAME_FLAG_NONE,
                "rb08r-nested-incoming",
                b"incoming-rejected",
            )
            .expect_err(
                "AC5 (append_frame): full 256 + nested damaged old tail + valid S+1 must fail",
            );
            assert!(
                matches!(app_err, StorageBackendError::CorruptCommittedFrame { .. }),
                "{case_label}: expected CorruptCommittedFrame from append_frame, got {app_err:?}"
            );
            assert_eq!(app_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                snapshot_tree(normal_dir),
                full_lane_snapshot_before,
                "{case_label}: AC6 violated after append_frame — normal/ mutated"
            );
            assert_eq!(
                snapshot_tree(&init.paths.quarantine),
                full_quarantine_snapshot_before,
                "{case_label}: AC6 violated after append_frame — quarantine/ mutated"
            );
        }
    }
}
