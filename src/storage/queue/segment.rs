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
    path::Path,
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
        FrameStreamItem, LTM2_MAX_LEN, LaneTailMeta, Ltm2CodecError, QUEUE_LANE_NORMAL_DIR,
        QUEUE_LANE_PRIORITY_DIR, QUEUE_TAIL_META_BAK_FILE, QUEUE_TAIL_META_FILE, crc32c_append,
        decode_frame, encode_frame, encode_postcard_frame, map_queue_owner_mismatch_err,
        map_queue_symlink_err, matches_segment_log_glob_bytes, segment_log_file_name,
    },
};

/// Staging filename used during atomic replacement of `tail.meta` (`ADR-RB-05` #3, `RB-08`).
pub const QUEUE_TAIL_META_TMP_FILE: &str = "tail.meta.tmp";

/// Staging filename used during atomic replacement of `tail.meta.bak` (`ADR-RB-05` #3, `RB-08`).
pub const QUEUE_TAIL_META_BAK_TMP_FILE: &str = "tail.meta.bak.tmp";

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
    pub reclaimed_orphans_removed: usize,
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
    /// Case C (clean synced tail): `max_live_seg == active_seg`,
    /// `actual_len > committed_len`, `[0, committed_len)` prefix CRC32C
    /// verified, and every byte in `[committed_len, actual_len)` decoded into
    /// complete dual-CRC32C valid frames (`ADR-RB-05` #3, `RB-08` AC5–AC6).
    CaseCRecoverCleanTail {
        meta: LaneTailMeta,
        recovered_valid_end: u64,
        recovered_crc32c: u32,
        recovered_frames: usize,
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
}

#[cfg(test)]
type BeforeCreateMissingLockHook = Box<dyn FnMut()>;

#[cfg(test)]
struct SegmentObserver {
    events: Vec<SegmentOpEvent>,
    crash_point: SegmentCrashPoint,
    before_create_missing_lock: Option<BeforeCreateMissingLockHook>,
}

#[cfg(test)]
impl SegmentObserver {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            crash_point: SegmentCrashPoint::None,
            before_create_missing_lock: None,
        }
    }

    fn with_crash(crash_point: SegmentCrashPoint) -> Self {
        Self {
            events: Vec::new(),
            crash_point,
            before_create_missing_lock: None,
        }
    }

    fn with_before_create_missing_lock(hook: impl FnMut() + 'static) -> Self {
        Self {
            events: Vec::new(),
            crash_point: SegmentCrashPoint::None,
            before_create_missing_lock: Some(Box::new(hook)),
        }
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
    fn record_release_lock(&mut self) {}

    #[inline]
    fn maybe_crash(&self, _point: SegmentCrashPoint) -> StorageResult<()> {
        Ok(())
    }

    #[inline]
    fn on_before_create_missing_lock(&mut self) {}
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
}

/// Forward-scan `[committed_len, actual_len)` on `file` (already positioned at
/// `committed_len` after [`verify_committed_prefix_crc`]) to verify all
/// complete dual-CRC32C frames written after a crash (`ADR-RB-05` #3 Case C).
fn scan_case_c_clean_uncommitted_tail(
    file: &mut File,
    committed_len: u64,
    actual_len: u64,
    prefix_crc32c: u32,
) -> StorageResult<CaseCTailScan> {
    debug_assert!(actual_len > committed_len);
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

    if damaged_tail || valid_end < actual_len {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "capture queue active segment has damaged uncommitted tail requiring durable quarantine recovery"
                    .to_string(),
        });
    }

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
    })
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
/// `<storage_dir>/private/capture-queue/<lane>/` (`ADR-RB-05` #3, `RB-08`).
pub fn append_frame(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_frame(flags, dedup_key, postcard_envelope)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir(storage_dir, lane, &frame_buf, &mut observer)
}

/// Encode a single capture frame outside `append.lock` and durably append it to
/// `<queue_root>/<lane>/` (`ADR-RB-05` #3, `RB-08`).
pub fn append_frame_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_frame(flags, dedup_key, postcard_envelope)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_at_root(queue_root, lane, &frame_buf, &mut observer)
}

/// Serialize `value` with versioned `postcard` framing outside `append.lock`
/// and durably append it to `<storage_dir>/private/capture-queue/<lane>/`
/// (`ADR-RB-05` #3, `RB-08`).
pub fn append_postcard_frame<T: Serialize>(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    flags: u8,
    dedup_key: &str,
    schema_version: u16,
    value: &T,
) -> StorageResult<SegmentAppendOutcome> {
    let frame_buf = encode_postcard_frame(flags, dedup_key, schema_version, value)
        .map_err(FrameEncodeError::into_storage_error)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir(storage_dir, lane, &frame_buf, &mut observer)
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<storage_dir>/private/capture-queue/<lane>/` (`ADR-RB-05` #3, `RB-08`).
pub fn append_encoded_frame(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    validate_single_encoded_frame(encoded_frame)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_in_storage_dir(storage_dir, lane, encoded_frame, &mut observer)
}

/// Validate a pre-encoded `LCE1` frame outside `append.lock` and durably append
/// it to `<queue_root>/<lane>/` (`ADR-RB-05` #3, `RB-08`).
pub fn append_encoded_frame_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    encoded_frame: &[u8],
) -> StorageResult<SegmentAppendOutcome> {
    validate_single_encoded_frame(encoded_frame)?;
    let mut observer = SegmentObserver::new();
    append_validated_frame_at_root(queue_root, lane, encoded_frame, &mut observer)
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

fn append_validated_frame_in_storage_dir(
    storage_dir: &Path,
    lane: CaptureQueueLane,
    frame_buf: &[u8],
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentAppendOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::append_in_storage_dir_unix(storage_dir, lane, frame_buf, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::append_in_storage_dir_non_unix(storage_dir, lane, frame_buf, observer)
    }
}

fn append_validated_frame_at_root(
    queue_root: &Path,
    lane: CaptureQueueLane,
    frame_buf: &[u8],
    observer: &mut SegmentObserver,
) -> StorageResult<SegmentAppendOutcome> {
    #[cfg(unix)]
    {
        let owners = UnixExpectedQueueOwners::current_effective();
        unix_seg::append_at_root_unix(queue_root, lane, frame_buf, owners, observer)
    }
    #[cfg(not(unix))]
    {
        non_unix_seg::append_at_root_non_unix(queue_root, lane, frame_buf, observer)
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

    /// Open an already-initialized `append.lock` strictly without `O_CREAT` or
    /// `fchmod`, verifying non-symlink, `S_IFREG`, effective UID, and exact
    /// `0600` mode, then acquire `flock(LOCK_EX)`.
    ///
    /// Returns `Ok(None)` when `append.lock` is absent (`ENOENT`), letting the
    /// caller run read-only Phase 1 classification before deciding whether to
    /// create `append.lock` so fail-closed classifications leave disk 100%
    /// unchanged.
    fn try_acquire_existing_strict_append_lock_unix(
        root_fd: &OwnedFd,
        expected_uid: u32,
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
        loop {
            // SAFETY: `lock_fd` is a valid open regular file descriptor.
            let rc = unsafe { libc::flock(lock_fd.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(StorageBackendError::io(
                "lock capture queue append.lock",
                err,
            ));
        }
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
                loop {
                    // SAFETY: `lock_fd` is a valid open regular file descriptor.
                    let rc = unsafe { libc::flock(lock_fd.as_raw_fd(), libc::LOCK_EX) };
                    if rc == 0 {
                        break;
                    }
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(StorageBackendError::io(
                        "lock newly created capture queue append.lock",
                        err,
                    ));
                }
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
                    try_acquire_existing_strict_append_lock_unix(root_fd, expected_uid)?
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

    /// Acquire `append.lock` and execute Phase 1 strictly read-only
    /// classification without ever mutating disk before a fail-closed error:
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
        observer: &mut SegmentObserver,
    ) -> StorageResult<(AppendLockGuard, Phase1Plan<OsString>)> {
        if let Some(lock) = try_acquire_existing_strict_append_lock_unix(root_fd, owners.root_uid)?
        {
            observer.record_acquire_lock();
            let plan = phase1_classify_lane_unix(lane_fd, lane_uid)?;
            return Ok((lock, plan));
        }

        // `append.lock` is absent: pre-classify strictly read-only so any
        // fail-closed state exits with zero filesystem mutation.
        let _ = phase1_classify_lane_unix(lane_fd, lane_uid)?;
        observer.on_before_create_missing_lock();

        let lock = create_or_acquire_strict_append_lock_unix(root_fd, owners.root_uid)?;
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
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail:
                    "Case D: orphan successor segment present beyond active_seg (requires Case D recovery)"
                        .to_string(),
            });
        }

        if actual_len == meta.committed_len {
            Ok(Phase1Plan::CaseAClean {
                meta,
                mirror_sync,
                stale_tmp_names: scan.stale_tmp_names,
                reclaimed_orphans,
            })
        } else {
            let tail = scan_case_c_clean_uncommitted_tail(
                &mut ro_seg,
                meta.committed_len,
                actual_len,
                meta.prefix_crc32c,
            )?;
            Ok(Phase1Plan::CaseCRecoverCleanTail {
                meta,
                recovered_valid_end: tail.valid_end,
                recovered_crc32c: tail.recovered_crc32c,
                recovered_frames: tail.frame_count,
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
    }

    fn phase2_execute_plan_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        plan: Phase1Plan<OsString>,
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<Phase2ConvergedState> {
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
                })
            }
            Phase1Plan::CaseCRecoverCleanTail {
                meta,
                recovered_valid_end,
                recovered_crc32c,
                recovered_frames,
                stale_tmp_names,
                reclaimed_orphans,
            } => {
                let cleaned_tmp_files =
                    clean_stale_tmp_files_unix(lane_fd, &stale_tmp_names, expected_uid)?;
                let reclaimed_orphans_removed =
                    clean_reclaimed_orphans_unix(lane_fd, &reclaimed_orphans, expected_uid)?;
                let recovered_bytes = recovered_valid_end
                    .checked_sub(meta.committed_len)
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "case C recovered length underflow".to_string(),
                    })?;
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
                    recovered_frames,
                    recovered_bytes,
                    mirrored_replica: true,
                    cleaned_tmp_files,
                    reclaimed_orphans_removed,
                })
            }
        }
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

    fn open_or_create_active_segment_unix(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        seg_id: u32,
        create_new: bool,
        expected_len: u64,
        expected_uid: u32,
        observer: &mut SegmentObserver,
    ) -> StorageResult<File> {
        let seg_name = segment_log_file_name(seg_id);
        let cname = osstr_to_cstring(OsStr::new(&seg_name), "open active capture queue segment")?;

        let raw_fd = if create_new {
            let flags =
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            unsafe {
                libc::openat(
                    lane_fd.as_raw_fd(),
                    cname.as_ptr(),
                    flags,
                    QUEUE_FILE_MODE as libc::c_uint,
                )
            }
        } else {
            let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
            // SAFETY: `lane_fd` is valid and `cname` is NUL-terminated.
            unsafe { libc::openat(lane_fd.as_raw_fd(), cname.as_ptr(), flags) }
        };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, lane_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue segment file"));
            }
            return Err(StorageBackendError::io(
                "open or create active capture queue segment",
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
        if create_new || unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
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
        if create_new {
            observer.record_create_segment(lane_label, seg_id);
            fsync_fd(
                lane_fd,
                "fsync capture queue lane directory after creating segment",
            )?;
            observer.record_fsync_lane_after_create_segment(lane_label, seg_id);
        }
        Ok(File::from(fd))
    }

    fn append_under_root_fd_unix(
        root_fd: &OwnedFd,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let lane_label = lane_dir_name(lane);
        let lane_uid = lane_expected_uid(owners, lane);
        let lane_fd = open_lane_dir_fd(root_fd, lane, owners)?;

        // Phase 1: acquire lock without pre-classification disk mutation and
        // execute strictly 100% read-only classification.
        let (append_lock, plan) =
            acquire_lock_and_phase1_unix(root_fd, &lane_fd, owners, lane_uid, observer)?;

        // Phase 2: clean stale `.tmp`, mirror lagging replica, recover Case C.
        let converged = phase2_execute_plan_unix(&lane_fd, lane_label, plan, lane_uid, observer)?;

        let seg_id = converged.meta.active_seg;
        let frame_offset = converged.meta.committed_len;
        let mut seg_file = open_or_create_active_segment_unix(
            &lane_fd,
            lane_label,
            seg_id,
            !converged.active_seg_exists,
            frame_offset,
            lane_uid,
            observer,
        )?;

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

        let frame_len_u64 =
            u64::try_from(frame_buf.len()).map_err(|_| StorageBackendError::BackendInternal {
                detail: "frame length exceeds u64".to_string(),
            })?;
        let next_committed_len = frame_offset.checked_add(frame_len_u64).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "capture queue segment committed_len overflow".to_string(),
            }
        })?;
        let next_prefix_crc = crc32c_append(converged.meta.prefix_crc32c, frame_buf);
        let next_meta = LaneTailMeta {
            min_live_seg: converged.meta.min_live_seg,
            active_seg: seg_id,
            committed_len: next_committed_len,
            prefix_crc32c: next_prefix_crc,
            sealed: converged.meta.sealed,
        };

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
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        let lane_label = lane_dir_name(lane);
        let lane_uid = lane_expected_uid(owners, lane);
        let lane_fd = open_lane_dir_fd(root_fd, lane, owners)?;

        let (append_lock, plan) =
            acquire_lock_and_phase1_unix(root_fd, &lane_fd, owners, lane_uid, observer)?;
        let converged = phase2_execute_plan_unix(&lane_fd, lane_label, plan, lane_uid, observer)?;

        drop(append_lock);
        observer.record_release_lock();

        Ok(LaneRecoveryReport {
            lane,
            meta: converged.meta,
            recovered_frames: converged.recovered_frames,
            recovered_bytes: converged.recovered_bytes,
            mirrored_replica: converged.mirrored_replica,
            cleaned_tmp_files: converged.cleaned_tmp_files,
            reclaimed_orphans_removed: converged.reclaimed_orphans_removed,
        })
    }

    pub(super) fn append_in_storage_dir_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        append_under_root_fd_unix(&root_fd, lane, frame_buf, owners, observer)
    }

    pub(super) fn append_at_root_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        let (root_fd, root_st) =
            open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory")?;
        verify_dir_mode_0700(&root_st, "capture queue root directory")?;
        append_under_root_fd_unix(&root_fd, lane, frame_buf, owners, observer)
    }

    pub(super) fn recover_in_storage_dir_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        owners: UnixExpectedQueueOwners,
        observer: &mut SegmentObserver,
    ) -> StorageResult<LaneRecoveryReport> {
        let root_fd = open_root_from_storage_dir(storage_dir, owners)?;
        recover_under_root_fd_unix(&root_fd, lane, owners, observer)
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
        recover_under_root_fd_unix(&root_fd, lane, owners, observer)
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

    #[cfg(windows)]
    mod win_impl {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                GetFileInformationByHandle, OPEN_EXISTING,
            },
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
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail:
                        "Case D: orphan successor segment present beyond active_seg (requires Case D recovery)"
                            .to_string(),
                });
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
                let tail = scan_case_c_clean_uncommitted_tail(
                    &mut seg_file,
                    meta.committed_len,
                    actual_len,
                    meta.prefix_crc32c,
                )?;
                Ok((
                    Phase1Plan::CaseCRecoverCleanTail {
                        meta,
                        recovered_valid_end: tail.valid_end,
                        recovered_crc32c: tail.recovered_crc32c,
                        recovered_frames: tail.frame_count,
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

        fn acquire_existing_lock_windows(queue_root: &Path) -> StorageResult<Option<File>> {
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
                    file.lock().map_err(|e| {
                        StorageBackendError::io("lock capture queue append.lock", e)
                    })?;
                    Ok(Some(file))
                }
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => Ok(None),
                Err(err) => Err(err),
            }
        }

        fn create_or_open_lock_windows(queue_root: &Path) -> StorageResult<File> {
            let lock_path = queue_root.join(QUEUE_APPEND_LOCK_FILE);
            for _ in 0..32 {
                if let Some(file) = acquire_existing_lock_windows(queue_root)? {
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
                        file.lock().map_err(|e| {
                            StorageBackendError::io("lock capture queue append.lock", e)
                        })?;
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
            observer: &mut SegmentObserver,
        ) -> StorageResult<(File, Phase1Plan<String>, Option<PinnedWindowsSegment>)> {
            if let Some(lock) = acquire_existing_lock_windows(queue_root)? {
                observer.record_acquire_lock();
                let (plan, pinned) = phase1_classify_lane_windows(lane_dir, open_seg_writable)?;
                return Ok((lock, plan, pinned));
            }
            // `append.lock` is absent: pre-classify read-only first so corrupt
            // lanes exit without creating `append.lock`.
            let _ = phase1_classify_lane_windows(lane_dir, false)?;
            let lock = create_or_open_lock_windows(queue_root)?;
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

        fn phase2_execute_plan_windows(
            lane_dir: &Path,
            lane_label: &'static str,
            plan: Phase1Plan<String>,
            observer: &mut SegmentObserver,
        ) -> StorageResult<(LaneTailMeta, bool, usize, u64, bool, usize, usize)> {
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
                    Ok((meta, false, 0, 0, false, cleaned, 0))
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
                    Ok((meta, false, 0, 0, mirrored, cleaned, 0))
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
                    Ok((meta, true, 0, 0, mirrored, cleaned, orphans))
                }
                Phase1Plan::CaseCRecoverCleanTail {
                    meta,
                    recovered_valid_end,
                    recovered_crc32c,
                    recovered_frames,
                    stale_tmp_names,
                    reclaimed_orphans,
                } => {
                    let cleaned = clean_tmp(&stale_tmp_names)?;
                    let orphans = clean_orphans(&reclaimed_orphans)?;
                    let recovered_bytes = recovered_valid_end
                        .checked_sub(meta.committed_len)
                        .ok_or_else(|| StorageBackendError::BackendInternal {
                            detail: "case C recovered length underflow".to_string(),
                        })?;
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
                    Ok((
                        next_meta,
                        true,
                        recovered_frames,
                        recovered_bytes,
                        true,
                        cleaned,
                        orphans,
                    ))
                }
            }
        }

        pub(super) fn append_in_storage_dir_windows(
            storage_dir: &Path,
            lane: CaptureQueueLane,
            frame_buf: &[u8],
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentAppendOutcome> {
            let private_dir = storage_dir.join(PRIVATE_SUBDIR);
            let _private_guard =
                pin_windows_dir_nofollow(&private_dir, "capture private directory")?;
            let queue_root = private_dir.join(CAPTURE_QUEUE_SUBDIR);
            append_at_root_windows(&queue_root, lane, frame_buf, observer)
        }

        pub(super) fn append_at_root_windows(
            queue_root: &Path,
            lane: CaptureQueueLane,
            frame_buf: &[u8],
            observer: &mut SegmentObserver,
        ) -> StorageResult<SegmentAppendOutcome> {
            let _root_guard = pin_windows_dir_nofollow(queue_root, "capture queue root directory")?;
            let lane_label = lane_dir_name(lane);
            let lane_dir = queue_root.join(lane_label);
            let _lane_guard = pin_windows_dir_nofollow(&lane_dir, "capture queue lane directory")?;

            let (append_lock, plan, pinned_seg) =
                acquire_lock_and_phase1_windows(queue_root, &lane_dir, true, observer)?;
            let (meta, active_seg_exists, recovered_frames, _, _, _, _) =
                phase2_execute_plan_windows(&lane_dir, lane_label, plan, observer)?;

            let seg_id = meta.active_seg;
            let frame_offset = meta.committed_len;
            let mut seg_file = if !active_seg_exists {
                let seg_path = lane_dir.join(segment_log_file_name(seg_id));
                let (file, size, _) = open_windows_file_nofollow(
                    &seg_path,
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    CREATE_NEW,
                    "capture queue segment file",
                    "create active capture queue segment",
                )?;
                if size != 0 {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: "newly created active capture queue segment is non-empty"
                            .to_string(),
                    });
                }
                observer.record_create_segment(lane_label, seg_id);
                flush_windows_dir(&lane_dir)?;
                observer.record_fsync_lane_after_create_segment(lane_label, seg_id);
                file
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

            let frame_len_u64 = u64::try_from(frame_buf.len()).map_err(|_| {
                StorageBackendError::BackendInternal {
                    detail: "frame length exceeds u64".to_string(),
                }
            })?;
            let next_committed_len = frame_offset.checked_add(frame_len_u64).ok_or_else(|| {
                StorageBackendError::BackendInternal {
                    detail: "capture queue segment committed_len overflow".to_string(),
                }
            })?;
            let next_prefix_crc = crc32c_append(meta.prefix_crc32c, frame_buf);
            let next_meta = LaneTailMeta {
                min_live_seg: meta.min_live_seg,
                active_seg: seg_id,
                committed_len: next_committed_len,
                prefix_crc32c: next_prefix_crc,
                sealed: meta.sealed,
            };
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

            let (append_lock, plan, _pinned) =
                acquire_lock_and_phase1_windows(queue_root, &lane_dir, false, observer)?;
            let (
                meta,
                _,
                recovered_frames,
                recovered_bytes,
                mirrored_replica,
                cleaned_tmp_files,
                reclaimed_orphans_removed,
            ) = phase2_execute_plan_windows(&lane_dir, lane_label, plan, observer)?;

            drop(append_lock);
            observer.record_release_lock();

            Ok(LaneRecoveryReport {
                lane,
                meta,
                recovered_frames,
                recovered_bytes,
                mirrored_replica,
                cleaned_tmp_files,
                reclaimed_orphans_removed,
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
    pub(super) fn append_in_storage_dir_non_unix(
        storage_dir: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        win_impl::append_in_storage_dir_windows(storage_dir, lane, frame_buf, observer)
    }

    #[cfg(windows)]
    pub(super) fn append_at_root_non_unix(
        queue_root: &Path,
        lane: CaptureQueueLane,
        frame_buf: &[u8],
        observer: &mut SegmentObserver,
    ) -> StorageResult<SegmentAppendOutcome> {
        win_impl::append_at_root_windows(queue_root, lane, frame_buf, observer)
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
}

// ============================================================================
// Tests (`RB-08` AC1–AC6)
// ============================================================================

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        fs,
        process::Command,
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::*;
    use crate::storage::queue::{
        DecodedFrame, FRAME_BODY_FIXED_OVERHEAD, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
        FRAME_HEADER_LEN, SealedSegmentMeta, crc32c_checksum, init_capture_queue,
        tail_meta_bak_path, tail_meta_path,
    };

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
}
