//! Dual-lane CRC32C capture frame queue (`ADR-RB-05`).
//!
//! Houses:
//! - [`frame`]: self-authenticating 16-byte header + dual Castagnoli CRC32C
//!   frame wire codec and mandatory-boundary sequential stream decoder (`RB-07`,
//!   `RB-07g`, `RB-07c`, `RB-07f`, `RB-07d`).
//! - Dual-lane secure directory initialization (`priority/`, `normal/`,
//!   `quarantine/` at `0700`, `O_NOFOLLOW` symlink rejection, Unix effective
//!   UID ownership verification) and `LTM2` dual-replica (`tail.meta` +
//!   `tail.meta.bak`) codec & fresh empty-lane pre-creation with immediate
//!   `fsync_dir(lane_dir)` (`RB-07b`).
//! - Subsequent Phase 2 cards (`RB-07e`..`RB-09l`) add fresh-Redb `capture-spool`
//!   `0400` tombstones, concurrent segment append & crash recovery, durable
//!   quarantine primitives, and single-writer priority-first consumption.

#[cfg(unix)]
use std::{
    ffi::{CStr, CString, OsStr, OsString},
    io,
    os::unix::{
        ffi::OsStrExt as _,
        io::{AsRawFd as _, FromRawFd as _, OwnedFd},
    },
};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(not(unix))]
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
};

use thiserror::Error;

use crate::storage::{
    capture_store::CaptureQueueLane,
    error::{StorageBackendError, StorageResult},
    manifest::PRIVATE_SUBDIR,
};

pub mod frame;
pub mod segment;

pub use frame::{
    DecodedFrame, FRAME_BODY_FIXED_OVERHEAD, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
    FRAME_HEADER_LEN, FRAME_MAGIC, FrameDecodeError, FrameEncodeError, FrameStreamDecoder,
    FrameStreamItem, MAX_FRAME_BODY_LEN, MAX_FRAME_DEDUP_KEY_LEN, MAX_FRAME_PAYLOAD_LEN,
    MIN_FRAME_BODY_LEN, ParsedFrameHeader, crc32c_append, crc32c_checksum, decode_frame,
    decode_postcard_payload, encode_frame, encode_postcard_frame, parse_and_verify_header,
    validate_frame_body_len,
};
pub use segment::{
    LaneRecoveryReport, QUEUE_TAIL_META_BAK_TMP_FILE, QUEUE_TAIL_META_TMP_FILE,
    SegmentAppendOutcome, append_encoded_frame, append_encoded_frame_at_root, append_frame,
    append_frame_at_root, append_postcard_frame, is_lane_meta_temp_name,
    parse_segment_log_file_name, read_converged_lane_tail_meta, recover_lane, recover_lane_at_root,
};

/// Subdirectory under `.libra/private/` housing the dual-lane CRC32C capture queue.
pub const CAPTURE_QUEUE_SUBDIR: &str = "capture-queue";

/// Priority-lane subdirectory (`SessionEnd` and terminal capture events).
pub const QUEUE_LANE_PRIORITY_DIR: &str = "priority";

/// Normal-lane subdirectory (all non-terminal capture events).
pub const QUEUE_LANE_NORMAL_DIR: &str = "normal";

/// Shared durable quarantine subdirectory for isolated uncommitted tail fragments.
pub const QUEUE_QUARANTINE_DIR: &str = "quarantine";

/// Primary lane tail metadata filename (`LTM2` format).
pub const QUEUE_TAIL_META_FILE: &str = "tail.meta";

/// Backup lane tail metadata filename (`LTM2` format).
pub const QUEUE_TAIL_META_BAK_FILE: &str = "tail.meta.bak";

/// Shared cross-lane append/capacity lock filename under `.libra/private/capture-queue/`.
pub const QUEUE_APPEND_LOCK_FILE: &str = "append.lock";

/// Subdirectory under `.libra/private/` housing the legacy SQLite capture spool
/// (tombstoned on fresh Redb repositories by `init_redb_capture_spool_tombstones`, `RB-07e`).
pub const CAPTURE_SPOOL_SUBDIR: &str = "capture-spool";

/// Capacity lock filename inside `.libra/private/capture-spool/` (`RB-07e`).
pub const CAPTURE_SPOOL_CAPACITY_LOCK_FILE: &str = "capacity.lock";

/// Legacy spool subdirectory names replaced by `0400` regular-file tombstones
/// on fresh Redb repositories (`ADR-RB-02` #1, `RB-07e`).
pub const CAPTURE_SPOOL_TOMBSTONE_NAMES: [&str; 3] = ["tmp", "ready", "quarantine"];

/// Fixed non-empty sentinel written into `capture-spool/{tmp,ready,quarantine}`
/// `0400` regular-file tombstones so legacy `spool.rs:330` `ensure_spool` fails
/// closed (`!is_dir()`) from day one (`ADR-RB-02` #1, `RB-07e`).
pub const CAPTURE_SPOOL_TOMBSTONE_SENTINEL: &[u8] = b"LIBRA_CAPTURE_SPOOL_FENCED_REDB\n";

/// Exact Unix permission bits required for capture queue directories (`0700`, `RB-07b`).
#[cfg(unix)]
pub const QUEUE_DIR_MODE: u32 = 0o700;

/// Exact Unix permission bits required for capture queue metadata and segment files (`0600`, `RB-07b`).
#[cfg(unix)]
pub const QUEUE_FILE_MODE: u32 = 0o600;

/// Exact Unix permission bits required for fresh-Redb `capture-spool` regular-file
/// tombstones (`0400`, `ADR-RB-02` #1, `RB-07e`).
#[cfg(unix)]
pub const CAPTURE_SPOOL_TOMBSTONE_MODE: u32 = 0o400;

/// Stable sanitized error prefix surfaced when `O_NOFOLLOW` rejects a symlink
/// in the capture queue directory tree or metadata files (`RB-07b`).
pub const CAPTURE_QUEUE_SYMLINK_ERROR_PREFIX: &str = "capture queue symlink rejected";

/// Stable sanitized error prefix surfaced when a capture queue directory or
/// metadata file is not owned by the current effective Unix UID (`RB-07b`).
pub const CAPTURE_QUEUE_OWNER_ERROR_PREFIX: &str = "capture queue owner mismatch";

/// Returns `true` when `err` represents a Unix `O_NOFOLLOW` symlink rejection
/// on the capture queue directory tree or metadata files (`RB-07b`).
#[must_use]
pub fn is_capture_queue_symlink_rejected_error(err: &StorageBackendError) -> bool {
    matches!(
        err,
        StorageBackendError::BackendInternal { detail }
            if detail.starts_with(CAPTURE_QUEUE_SYMLINK_ERROR_PREFIX)
    )
}

/// Returns `true` when `err` represents a Unix effective-UID ownership mismatch
/// on the capture queue directory tree or metadata files (`RB-07b`).
#[must_use]
pub fn is_capture_queue_owner_mismatch_error(err: &StorageBackendError) -> bool {
    matches!(
        err,
        StorageBackendError::BackendInternal { detail }
            if detail.starts_with(CAPTURE_QUEUE_OWNER_ERROR_PREFIX)
    )
}

pub(super) fn map_queue_symlink_err(subject: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!("{CAPTURE_QUEUE_SYMLINK_ERROR_PREFIX}: {subject}"),
    }
}

pub(super) fn map_queue_owner_mismatch_err(subject: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!("{CAPTURE_QUEUE_OWNER_ERROR_PREFIX}: {subject}"),
    }
}

// ============================================================================
// LTM2 Dual-Replica Lane Tail Metadata Wire Format (`ADR-RB-05` #3, `RB-07b`)
// ============================================================================

/// 4-byte wire magic identifying Lane Tail Metadata format v2 (`LTM2`).
pub const LTM2_MAGIC: [u8; 4] = *b"LTM2";

/// Protocol version number stored in `LTM2` bytes `4..6`.
pub const LTM2_VERSION: u16 = 2;

/// Fixed byte length of the `LTM2` header before `sealed[]` entries:
/// `MAGIC(4) + VERSION(2) + MIN_LIVE_SEG(4) + ACTIVE_SEG(4) + COMMITTED_LEN(8)
/// + PREFIX_CRC32C(4) + SEALED_COUNT(4) = 30`.
pub const LTM2_HEADER_LEN: usize = 30;

/// Fixed byte length of a single `LTM2` `sealed[]` entry:
/// `SEG_ID(4) + SEALED_LEN(8) + SEALED_CRC32C(4) = 16`.
pub const LTM2_SEALED_ENTRY_LEN: usize = 16;

/// Fixed byte length of the trailing `META_CRC32C` (`4`).
pub const LTM2_TRAILER_LEN: usize = 4;

/// Exact byte length of an initial empty `LTM2` record (`sealed = []`): `34` bytes.
pub const LTM2_MIN_LEN: usize = LTM2_HEADER_LEN + LTM2_TRAILER_LEN;

/// Hard maximum number of un-reclaimed sealed segments recorded in `LTM2.sealed[]`
/// (`ADR-RB-05` #3).
pub const MAX_SEALED_SEGMENTS: usize = 256;

/// Maximum legal encoded byte length of an `LTM2` record (`34 + 16 * 256 = 4130`).
pub const LTM2_MAX_LEN: usize = LTM2_MIN_LEN + MAX_SEALED_SEGMENTS * LTM2_SEALED_ENTRY_LEN;

const _: () = assert!(LTM2_HEADER_LEN == 30, "LTM2 header must be 30 bytes");
const _: () = assert!(LTM2_MIN_LEN == 34, "initial empty LTM2 must be 34 bytes");
const _: () = assert!(
    LTM2_MAX_LEN == 34 + 16 * 256,
    "LTM2 max length must equal 34 + 16 * 256"
);

/// Certified boundary of a single un-reclaimed rotated segment (`min_live_seg <= seg_id < active_seg`)
/// persisted inside `LTM2.sealed[]` (`ADR-RB-05` #3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedSegmentMeta {
    pub seg_id: u32,
    pub sealed_len: u64,
    pub sealed_crc32c: u32,
}

/// Decoded `LTM2` lane tail metadata boundary persisted identically across
/// `tail.meta` and `tail.meta.bak` (`ADR-RB-05` #3, `RB-07b`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneTailMeta {
    pub min_live_seg: u32,
    pub active_seg: u32,
    pub committed_len: u64,
    pub prefix_crc32c: u32,
    pub sealed: Vec<SealedSegmentMeta>,
}

/// Errors returned when encoding or decoding an `LTM2` dual-replica metadata record.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Ltm2CodecError {
    #[error("LTM2 metadata length invalid: {detail}")]
    InvalidLength { detail: &'static str },
    #[error("LTM2 metadata magic mismatch")]
    MagicMismatch,
    #[error("LTM2 metadata version unsupported ({version})")]
    UnsupportedVersion { version: u16 },
    #[error("LTM2 metadata CRC32C mismatch")]
    CrcMismatch,
    #[error("LTM2 metadata invariant violated: {detail}")]
    InvalidInvariant { detail: &'static str },
}

impl Ltm2CodecError {
    /// Convert into a sanitized [`StorageBackendError`] (`LBR-IO-001`).
    #[must_use]
    pub fn into_storage_error(self) -> StorageBackendError {
        StorageBackendError::BackendInternal {
            detail: self.to_string(),
        }
    }
}

impl LaneTailMeta {
    /// Construct the canonical initial 34-byte empty lane metadata
    /// `(min_live_seg=0, active_seg=0, committed_len=0, prefix_crc32c=0, sealed=[])`.
    #[must_use]
    pub const fn initial_empty() -> Self {
        Self {
            min_live_seg: 0,
            active_seg: 0,
            committed_len: 0,
            prefix_crc32c: 0,
            sealed: Vec::new(),
        }
    }

    /// Returns `true` iff this record equals the exact initial empty boundary
    /// `(0, 0, 0, 0, [])`.
    #[must_use]
    pub fn is_initial_empty(&self) -> bool {
        self.min_live_seg == 0
            && self.active_seg == 0
            && self.committed_len == 0
            && self.prefix_crc32c == 0
            && self.sealed.is_empty()
    }

    /// Strict total-order convergence key `(active_seg, committed_len, min_live_seg)`
    /// used to select the authoritative replica when both `tail.meta` and
    /// `tail.meta.bak` are valid (`ADR-RB-05` #3).
    #[must_use]
    pub const fn ordering_key(&self) -> (u32, u64, u32) {
        (self.active_seg, self.committed_len, self.min_live_seg)
    }

    /// Validate all structural invariants of this `LTM2` record.
    pub fn validate(&self) -> Result<(), Ltm2CodecError> {
        if self.min_live_seg > self.active_seg {
            return Err(Ltm2CodecError::InvalidInvariant {
                detail: "min_live_seg exceeds active_seg",
            });
        }
        let expected_sealed = (self.active_seg - self.min_live_seg) as usize;
        if expected_sealed > MAX_SEALED_SEGMENTS || self.sealed.len() > MAX_SEALED_SEGMENTS {
            return Err(Ltm2CodecError::InvalidInvariant {
                detail: "sealed segment count exceeds MAX_SEALED_SEGMENTS (256)",
            });
        }
        if self.sealed.len() != expected_sealed {
            return Err(Ltm2CodecError::InvalidInvariant {
                detail: "sealed segment count does not match active_seg - min_live_seg",
            });
        }
        for (idx, entry) in self.sealed.iter().enumerate() {
            let expected_seg_id = self.min_live_seg.checked_add(idx as u32).ok_or(
                Ltm2CodecError::InvalidInvariant {
                    detail: "sealed segment id overflow",
                },
            )?;
            if entry.seg_id != expected_seg_id {
                return Err(Ltm2CodecError::InvalidInvariant {
                    detail: "sealed segment ids are not strictly contiguous ascending",
                });
            }
        }
        Ok(())
    }

    /// Encode this `LTM2` record into its exact `34 + 16 * sealed.len()`-byte
    /// wire representation with trailing Castagnoli `META_CRC32C`.
    pub fn encode(&self) -> Result<Vec<u8>, Ltm2CodecError> {
        self.validate()?;
        let sealed_count =
            u32::try_from(self.sealed.len()).map_err(|_| Ltm2CodecError::InvalidInvariant {
                detail: "sealed segment count overflows u32",
            })?;
        let total_len = LTM2_MIN_LEN + self.sealed.len() * LTM2_SEALED_ENTRY_LEN;
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(&LTM2_MAGIC);
        buf.extend_from_slice(&LTM2_VERSION.to_le_bytes());
        buf.extend_from_slice(&self.min_live_seg.to_le_bytes());
        buf.extend_from_slice(&self.active_seg.to_le_bytes());
        buf.extend_from_slice(&self.committed_len.to_le_bytes());
        buf.extend_from_slice(&self.prefix_crc32c.to_le_bytes());
        buf.extend_from_slice(&sealed_count.to_le_bytes());
        for entry in &self.sealed {
            buf.extend_from_slice(&entry.seg_id.to_le_bytes());
            buf.extend_from_slice(&entry.sealed_len.to_le_bytes());
            buf.extend_from_slice(&entry.sealed_crc32c.to_le_bytes());
        }
        let meta_crc = crc32c_checksum(&buf);
        buf.extend_from_slice(&meta_crc.to_le_bytes());
        debug_assert_eq!(buf.len(), total_len);
        Ok(buf)
    }

    /// Decode and strictly authenticate an `LTM2` binary record.
    pub fn decode(bytes: &[u8]) -> Result<Self, Ltm2CodecError> {
        if bytes.len() < LTM2_MIN_LEN {
            return Err(Ltm2CodecError::InvalidLength {
                detail: "buffer shorter than 34-byte minimum LTM2 record",
            });
        }
        if bytes.len() > LTM2_MAX_LEN {
            return Err(Ltm2CodecError::InvalidLength {
                detail: "buffer exceeds 4130-byte maximum LTM2 record",
            });
        }
        if bytes[0..4] != LTM2_MAGIC {
            return Err(Ltm2CodecError::MagicMismatch);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != LTM2_VERSION {
            return Err(Ltm2CodecError::UnsupportedVersion { version });
        }
        let min_live_seg = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let active_seg = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        let committed_len = u64::from_le_bytes([
            bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21],
        ]);
        let prefix_crc32c = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
        let sealed_count_u32 = u32::from_le_bytes([bytes[26], bytes[27], bytes[28], bytes[29]]);
        let sealed_count =
            usize::try_from(sealed_count_u32).map_err(|_| Ltm2CodecError::InvalidInvariant {
                detail: "sealed segment count exceeds platform usize",
            })?;
        if sealed_count > MAX_SEALED_SEGMENTS {
            return Err(Ltm2CodecError::InvalidInvariant {
                detail: "sealed segment count exceeds MAX_SEALED_SEGMENTS (256)",
            });
        }
        let expected_len = LTM2_MIN_LEN + sealed_count * LTM2_SEALED_ENTRY_LEN;
        if bytes.len() != expected_len {
            return Err(Ltm2CodecError::InvalidLength {
                detail: "buffer length does not match 34 + 16 * sealed_count",
            });
        }

        let crc_offset = bytes.len() - LTM2_TRAILER_LEN;
        let stored_crc = u32::from_le_bytes([
            bytes[crc_offset],
            bytes[crc_offset + 1],
            bytes[crc_offset + 2],
            bytes[crc_offset + 3],
        ]);
        let computed_crc = crc32c_checksum(&bytes[..crc_offset]);
        if stored_crc != computed_crc {
            return Err(Ltm2CodecError::CrcMismatch);
        }

        let mut sealed = Vec::with_capacity(sealed_count);
        for i in 0..sealed_count {
            let base = LTM2_HEADER_LEN + i * LTM2_SEALED_ENTRY_LEN;
            let seg_id = u32::from_le_bytes([
                bytes[base],
                bytes[base + 1],
                bytes[base + 2],
                bytes[base + 3],
            ]);
            let sealed_len = u64::from_le_bytes([
                bytes[base + 4],
                bytes[base + 5],
                bytes[base + 6],
                bytes[base + 7],
                bytes[base + 8],
                bytes[base + 9],
                bytes[base + 10],
                bytes[base + 11],
            ]);
            let sealed_crc32c = u32::from_le_bytes([
                bytes[base + 12],
                bytes[base + 13],
                bytes[base + 14],
                bytes[base + 15],
            ]);
            sealed.push(SealedSegmentMeta {
                seg_id,
                sealed_len,
                sealed_crc32c,
            });
        }

        let meta = Self {
            min_live_seg,
            active_seg,
            committed_len,
            prefix_crc32c,
            sealed,
        };
        meta.validate()?;
        Ok(meta)
    }
}

// ============================================================================
// Dual-Lane Directory Layout & Fresh Empty-Lane Initial LTM2 Pre-Creation
// ============================================================================

/// Resolved directory paths for a repository's dual-lane capture queue
/// (`.libra/private/capture-queue/{priority,normal,quarantine}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureQueuePaths {
    pub root: PathBuf,
    pub priority: PathBuf,
    pub normal: PathBuf,
    pub quarantine: PathBuf,
}

impl CaptureQueuePaths {
    /// Resolve `.libra/private/capture-queue/{priority,normal,quarantine}` from
    /// a repository storage directory (`.libra/`).
    #[must_use]
    pub fn from_storage_dir(storage_dir: &Path) -> Self {
        Self::from_queue_root(&storage_dir.join(PRIVATE_SUBDIR).join(CAPTURE_QUEUE_SUBDIR))
    }

    /// Resolve `{priority,normal,quarantine}` directly under `queue_root`.
    #[must_use]
    pub fn from_queue_root(queue_root: &Path) -> Self {
        Self {
            root: queue_root.to_path_buf(),
            priority: queue_root.join(QUEUE_LANE_PRIORITY_DIR),
            normal: queue_root.join(QUEUE_LANE_NORMAL_DIR),
            quarantine: queue_root.join(QUEUE_QUARANTINE_DIR),
        }
    }

    /// Return the directory path for `lane`.
    #[must_use]
    pub fn lane_dir(&self, lane: CaptureQueueLane) -> &Path {
        match lane {
            CaptureQueueLane::Priority => &self.priority,
            CaptureQueueLane::Normal => &self.normal,
        }
    }
}

/// Primary `tail.meta` path inside `lane_dir`.
#[must_use]
pub fn tail_meta_path(lane_dir: &Path) -> PathBuf {
    lane_dir.join(QUEUE_TAIL_META_FILE)
}

/// Backup `tail.meta.bak` path inside `lane_dir`.
#[must_use]
pub fn tail_meta_bak_path(lane_dir: &Path) -> PathBuf {
    lane_dir.join(QUEUE_TAIL_META_BAK_FILE)
}

/// Canonical segment filename (`events-<seg_id:08>.log`).
#[must_use]
pub fn segment_log_file_name(seg_id: u32) -> String {
    format!("events-{seg_id:08}.log")
}

/// Returns `true` if `name` matches the `events-*.log` segment naming pattern
/// checked during empty-lane classification (`ADR-RB-05` #3).
#[must_use]
pub fn matches_segment_log_glob(name: &str) -> bool {
    matches_segment_log_glob_bytes(name.as_bytes())
}

/// Raw-byte counterpart of [`matches_segment_log_glob`] so non-UTF-8 Unix
/// filenames such as `events-\xff.log` are never silently ignored as absent
/// during empty-lane classification (`RB-07b`).
#[must_use]
pub fn matches_segment_log_glob_bytes(name: &[u8]) -> bool {
    name.starts_with(b"events-") && name.ends_with(b".log") && name.len() >= b"events-.log".len()
}

/// Outcome of initializing a single capture queue lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneInitOutcome {
    /// Fresh empty lane (`0` `events-*.log` and neither `tail.meta` nor
    /// `tail.meta.bak` existed): initial 34B `LTM2` `(0, 0, 0, 0, [])` dual
    /// replicas were atomically published and `fsync_dir(lane_dir)` completed.
    InitializedFreshEmpty,
    /// Lane already contained segment files or metadata files; existing files
    /// were preserved untouched after security verification.
    PreservedExisting,
}

/// Result returned by [`init_capture_queue`] / [`init_capture_queue_at_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureQueueInitReport {
    pub paths: CaptureQueuePaths,
    pub priority_outcome: LaneInitOutcome,
    pub normal_outcome: LaneInitOutcome,
}

/// Monotonic per-process counter for collision-free `tail.meta.<pid>.<seq>.tmp`
/// staging filenames.
static QUEUE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_meta_temp_name(target_name: &str) -> String {
    let seq = QUEUE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{target_name}.{}.{seq}.tmp", std::process::id())
}

/// Internal durability observer used by tests to assert exact
/// `write/sync -> publish -> fsync_dir(lane_dir)` ordering and inject
/// `fsync_dir` failure faults.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum QueueInitEvent {
    PublishInitialMeta {
        lane: &'static str,
        file_name: &'static str,
    },
    FsyncLaneDir {
        lane: &'static str,
    },
}

#[cfg(test)]
type AfterPublishSlotHook = Box<dyn FnMut(&'static str, &'static str)>;

#[cfg(test)]
struct QueueInitObserver {
    events: Vec<QueueInitEvent>,
    fail_fsync_on_call: Option<usize>,
    fsync_calls: usize,
    after_publish_slot: Option<AfterPublishSlotHook>,
}

#[cfg(test)]
impl QueueInitObserver {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            fail_fsync_on_call: None,
            fsync_calls: 0,
            after_publish_slot: None,
        }
    }

    fn failing_fsync_at(call_index: usize) -> Self {
        Self {
            events: Vec::new(),
            fail_fsync_on_call: Some(call_index),
            fsync_calls: 0,
            after_publish_slot: None,
        }
    }

    fn with_after_publish_hook(hook: impl FnMut(&'static str, &'static str) + 'static) -> Self {
        Self {
            events: Vec::new(),
            fail_fsync_on_call: None,
            fsync_calls: 0,
            after_publish_slot: Some(Box::new(hook)),
        }
    }

    fn record_publish(&mut self, lane: &'static str, file_name: &'static str) {
        self.events
            .push(QueueInitEvent::PublishInitialMeta { lane, file_name });
        if let Some(hook) = self.after_publish_slot.as_mut() {
            hook(lane, file_name);
        }
    }

    fn before_fsync_lane(&mut self, lane: &'static str) -> StorageResult<()> {
        let idx = self.fsync_calls;
        self.fsync_calls += 1;
        self.events.push(QueueInitEvent::FsyncLaneDir { lane });
        if self.fail_fsync_on_call == Some(idx) {
            return Err(StorageBackendError::io(
                "injected fsync_dir failure on capture queue lane",
                std::io::Error::other("injected fsync_dir failure"),
            ));
        }
        Ok(())
    }
}

#[cfg(not(test))]
struct QueueInitObserver;

#[cfg(not(test))]
impl QueueInitObserver {
    const fn new() -> Self {
        Self
    }

    #[inline]
    fn record_publish(&mut self, _lane: &'static str, _file_name: &'static str) {}

    #[inline]
    fn before_fsync_lane(&mut self, _lane: &'static str) -> StorageResult<()> {
        Ok(())
    }
}

/// Expected effective Unix UIDs verified for each security-sensitive capture
/// queue component (`RB-07b` AC3).
///
/// Production code always constructs [`Self::current_effective`] via
/// `libc::geteuid()`. Tests may override individual fields to exercise
/// ownership-mismatch fail-closed paths without requiring `CAP_CHOWN`.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnixExpectedQueueOwners {
    pub(super) private_uid: u32,
    pub(super) root_uid: u32,
    pub(super) priority_uid: u32,
    pub(super) normal_uid: u32,
    pub(super) quarantine_uid: u32,
}

#[cfg(unix)]
impl UnixExpectedQueueOwners {
    pub(super) fn current_effective() -> Self {
        // SAFETY: `geteuid(2)` has no preconditions and never fails.
        let uid = unsafe { libc::geteuid() };
        Self {
            private_uid: uid,
            root_uid: uid,
            priority_uid: uid,
            normal_uid: uid,
            quarantine_uid: uid,
        }
    }
}

/// Resolved paths created/verified by [`init_redb_capture_spool_tombstones`]
/// (`ADR-RB-02` #1, `RB-07e`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpoolTombstonePaths {
    pub root: PathBuf,
    pub capacity_lock: PathBuf,
    pub tmp: PathBuf,
    pub ready: PathBuf,
    pub quarantine: PathBuf,
}

impl CaptureSpoolTombstonePaths {
    /// Resolve `.libra/private/capture-spool/{capacity.lock,tmp,ready,quarantine}`
    /// from a repository storage directory (`.libra/`).
    #[must_use]
    pub fn from_storage_dir(storage_dir: &Path) -> Self {
        let root = storage_dir.join(PRIVATE_SUBDIR).join(CAPTURE_SPOOL_SUBDIR);
        Self {
            capacity_lock: root.join(CAPTURE_SPOOL_CAPACITY_LOCK_FILE),
            tmp: root.join("tmp"),
            ready: root.join("ready"),
            quarantine: root.join("quarantine"),
            root,
        }
    }
}

/// Outcome of initializing fresh-Redb `capture-spool` `0400` tombstones (`RB-07e`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolTombstoneOutcome {
    /// One or more `0400` regular-file tombstones were newly installed and
    /// `fsync_dir` completed durably.
    InstalledFresh,
    /// All three `0400` tombstones were already installed and verified intact.
    AlreadyFenced,
}

/// Result returned by [`init_redb_capture_spool_tombstones`] (`RB-07e`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpoolTombstoneReport {
    pub paths: CaptureSpoolTombstonePaths,
    pub outcome: SpoolTombstoneOutcome,
}

/// Expected effective Unix UIDs checked during [`init_redb_capture_spool_tombstones`]
/// (`RB-07e`). Production always uses `libc::geteuid()`.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnixExpectedSpoolOwners {
    private_uid: u32,
    spool_uid: u32,
    lock_uid: u32,
    tombstone_uid: u32,
}

#[cfg(unix)]
impl UnixExpectedSpoolOwners {
    fn current_effective() -> Self {
        // SAFETY: `geteuid(2)` has no preconditions and never fails.
        let uid = unsafe { libc::geteuid() };
        Self {
            private_uid: uid,
            spool_uid: uid,
            lock_uid: uid,
            tombstone_uid: uid,
        }
    }
}

/// Initialize `.libra/private/capture-spool/` (`0700`), `capacity.lock` (`0600`),
/// and `{tmp,ready,quarantine}` `0400` regular-file tombstones + `fsync_dir` on
/// a fresh Redb repository (`ADR-RB-02` #1, `RB-07e`).
///
/// Legacy SQLite hooks calling `spool.rs:330` `ensure_spool` immediately fail
/// closed because `{tmp,ready,quarantine}` are `0400` regular files rather than
/// directories. Refuses to overwrite any existing live legacy spool directory,
/// symlink, foreign-owned entry, or unexpected regular file.
pub fn init_redb_capture_spool_tombstones(
    storage_dir: &Path,
) -> StorageResult<CaptureSpoolTombstoneReport> {
    let mut observer = QueueInitObserver::new();
    #[cfg(unix)]
    {
        init_redb_capture_spool_tombstones_unix(
            storage_dir,
            UnixExpectedSpoolOwners::current_effective(),
            &mut observer,
        )
    }
    #[cfg(not(unix))]
    {
        init_redb_capture_spool_tombstones_non_unix(storage_dir, &mut observer)
    }
}

/// Initialize a repository's dual-lane capture queue under
/// `<storage_dir>/private/capture-queue/{priority,normal,quarantine}` (`RB-07b`):
///
/// - Enforces exact `0700` Unix permissions on `private/`, `capture-queue/`,
///   `priority/`, `normal/`, and `quarantine/`.
/// - Rejects symlinks (`O_NOFOLLOW`) and non-effective-UID owners fail-closed.
/// - On every fresh empty lane (`0` `events-*.log` files and neither
///   `tail.meta` nor `tail.meta.bak` present), atomically creates `0600`
///   initial 34-byte `LTM2` `(0, 0, 0, 0, [])` dual replicas `tail.meta` and
///   `tail.meta.bak` before any segment file exists, followed immediately by
///   `fsync_dir(lane_dir)`.
pub fn init_capture_queue(storage_dir: &Path) -> StorageResult<CaptureQueueInitReport> {
    let mut observer = QueueInitObserver::new();
    #[cfg(unix)]
    {
        init_capture_queue_unix(
            storage_dir,
            UnixExpectedQueueOwners::current_effective(),
            &mut observer,
        )
    }
    #[cfg(not(unix))]
    {
        init_capture_queue_non_unix(storage_dir, &mut observer)
    }
}

/// Initialize a dual-lane capture queue directly at an explicit `queue_root`
/// directory (`<queue_root>/{priority,normal,quarantine}`) with the exact same
/// `0700`/`O_NOFOLLOW`/UID/initial-34B-`LTM2`/`fsync_dir` guarantees (`RB-07b`).
pub fn init_capture_queue_at_root(queue_root: &Path) -> StorageResult<CaptureQueueInitReport> {
    let mut observer = QueueInitObserver::new();
    #[cfg(unix)]
    {
        init_capture_queue_at_root_unix(
            queue_root,
            UnixExpectedQueueOwners::current_effective(),
            &mut observer,
        )
    }
    #[cfg(not(unix))]
    {
        init_capture_queue_at_root_non_unix(queue_root, &mut observer)
    }
}

// ============================================================================
// Unix Descriptor-Pinned, O_NOFOLLOW, 0700/0600, UID-Verified Implementation
// ============================================================================

#[cfg(unix)]
pub(super) mod unix_impl {
    use super::*;

    pub(super) struct DirStreamGuard {
        pub(super) ptr: *mut libc::DIR,
    }

    impl Drop for DirStreamGuard {
        fn drop(&mut self) {
            if !self.ptr.is_null() {
                // SAFETY: `ptr` was returned by `fdopendir` and is closed exactly once here.
                unsafe {
                    libc::closedir(self.ptr);
                }
            }
        }
    }

    pub(super) struct TempUnlinkGuard<'a> {
        dir_fd: &'a OwnedFd,
        cname: CString,
        active: bool,
    }

    impl<'a> TempUnlinkGuard<'a> {
        pub(super) fn new(dir_fd: &'a OwnedFd, cname: CString) -> Self {
            Self {
                dir_fd,
                cname,
                active: true,
            }
        }

        pub(super) fn disarm(&mut self) {
            self.active = false;
        }
    }

    impl Drop for TempUnlinkGuard<'_> {
        fn drop(&mut self) {
            if self.active {
                // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
                unsafe {
                    libc::unlinkat(self.dir_fd.as_raw_fd(), self.cname.as_ptr(), 0);
                }
            }
        }
    }

    pub(super) fn clear_errno() {
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        // SAFETY: `__error()` returns a valid thread-local pointer to `errno`.
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: `__errno_location()` returns a valid thread-local pointer to `errno`.
        unsafe {
            *libc::__errno_location() = 0;
        }
    }

    pub(super) fn osstr_to_cstring(s: &OsStr, context: &'static str) -> StorageResult<CString> {
        CString::new(s.as_bytes()).map_err(|_| StorageBackendError::BackendInternal {
            detail: format!("{context}: path component contains interior NUL byte"),
        })
    }

    pub(super) fn path_to_cstring(path: &Path, context: &'static str) -> StorageResult<CString> {
        osstr_to_cstring(path.as_os_str(), context)
    }

    pub(super) fn unix_perm_bits(mode: libc::mode_t) -> u32 {
        #[allow(clippy::useless_conversion)]
        let raw = u32::from(mode);
        raw & 0o7777
    }

    pub(super) fn fstat_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `fd` is a valid open descriptor and `st` is writable.
        let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        // SAFETY: `fstat` succeeded and fully initialized `st`.
        Ok(unsafe { st.assume_init() })
    }

    pub(super) fn fchmod_fd(fd: &OwnedFd, mode: u32, context: &'static str) -> StorageResult<()> {
        // SAFETY: `fd` is a valid open descriptor.
        let rc = unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        Ok(())
    }

    pub(super) fn fsync_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<()> {
        // SAFETY: `fd` is a valid open descriptor.
        let rc = unsafe { libc::fsync(fd.as_raw_fd()) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        Ok(())
    }

    pub(super) fn is_symlink_errno_at(err: &io::Error, dir_fd: &OwnedFd, cname: &CString) -> bool {
        match err.raw_os_error() {
            Some(libc::ELOOP) => true,
            // Some Unix kernels return ENOTDIR when `O_DIRECTORY | O_NOFOLLOW` hits a symlink.
            Some(libc::ENOTDIR) => {
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
                    (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub(super) fn is_symlink_errno_path(err: &io::Error, path: &Path) -> bool {
        match err.raw_os_error() {
            Some(libc::ELOOP) => true,
            Some(libc::ENOTDIR) => {
                std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
            }
            _ => false,
        }
    }

    pub(super) fn open_ancestor_dir(dir: &Path, context: &'static str) -> StorageResult<OwnedFd> {
        let cdir = path_to_cstring(dir, context)?;
        // SAFETY: `cdir` is NUL-terminated. Ancestor directories (e.g. macOS `/var -> /private/var`)
        // may traverse system symlinks while every queue component below is pinned with `O_NOFOLLOW`.
        let raw_fd = unsafe {
            libc::open(
                cdir.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if raw_fd < 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }

    pub(super) fn open_dir_nofollow(
        dir: &Path,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<(OwnedFd, libc::stat)> {
        let cdir = path_to_cstring(dir, "open capture queue directory")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `cdir` is NUL-terminated.
        let raw_fd = unsafe { libc::open(cdir.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_path(&err, dir) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io("open capture queue directory", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&dir_fd, "stat capture queue directory")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a directory"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        Ok((dir_fd, st))
    }

    pub(super) fn openat_dir_nofollow(
        parent_fd: &OwnedFd,
        dir_name: &OsStr,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<(OwnedFd, libc::stat)> {
        let cname = osstr_to_cstring(dir_name, "open capture queue directory")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `parent_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(parent_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, parent_fd, &cname) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io("open capture queue directory", err));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&dir_fd, "stat capture queue directory")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a directory"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        Ok((dir_fd, st))
    }

    /// Enforce `0700` on a newly `mkdirat`-created directory without ever
    /// following symlinks, resilient even under restrictive `umask(0777)` or
    /// inherited setgid bits (`0o2700`).
    /// Descriptor-pinned Linux fallback when a restrictive process `umask`
    /// (e.g. `0777`) stripped owner read bits during `mkdirat`, preventing
    /// `O_RDONLY` open. Uses `O_PATH | O_DIRECTORY | O_NOFOLLOW`, verifies
    /// `fstat` (`S_ISDIR` and `st_uid == expected_uid`) on the pinned fd, and
    /// updates permissions strictly via `/proc/self/fd/<fd>` (zero TOCTOU).
    #[cfg(target_os = "linux")]
    fn fix_newly_created_unreadable_dir_linux(
        parent_fd: &OwnedFd,
        dir_name: &OsStr,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<()> {
        let cname = osstr_to_cstring(dir_name, "fix capture queue directory mode")?;
        let path_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `parent_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(parent_fd.as_raw_fd(), cname.as_ptr(), path_flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, parent_fd, &cname) {
                return Err(map_queue_symlink_err(subject));
            }
            return Err(StorageBackendError::io(
                "open newly created capture queue directory O_PATH",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let path_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let pst = fstat_fd(&path_fd, "stat newly created capture queue directory")?;
        if (pst.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a directory"),
            });
        }
        if pst.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        let proc_path =
            CString::new(format!("/proc/self/fd/{}", path_fd.as_raw_fd())).map_err(|_| {
                StorageBackendError::BackendInternal {
                    detail: "capture queue proc fd path invalid".to_string(),
                }
            })?;
        // SAFETY: `proc_path` refers directly to our pinned `path_fd` whose
        // `S_ISDIR` and `st_uid == expected_uid` were just verified via `fstat`.
        let chmod_rc = unsafe { libc::chmod(proc_path.as_ptr(), QUEUE_DIR_MODE as libc::mode_t) };
        if chmod_rc != 0 {
            return Err(StorageBackendError::io(
                "enforce 0700 mode on newly created capture queue directory",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    fn ensure_subdir_nofollow(
        parent_fd: &OwnedFd,
        dir_name: &OsStr,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<OwnedFd> {
        let (dir_fd, st, created_now) =
            match openat_dir_nofollow(parent_fd, dir_name, expected_uid, subject) {
                Ok((fd, st)) => (fd, st, false),
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {
                    let cname = osstr_to_cstring(dir_name, "create capture queue directory")?;
                    // SAFETY: `parent_fd` is valid and `cname` is NUL-terminated.
                    let mkdir_rc = unsafe {
                        libc::mkdirat(
                            parent_fd.as_raw_fd(),
                            cname.as_ptr(),
                            QUEUE_DIR_MODE as libc::mode_t,
                        )
                    };
                    let created_now = if mkdir_rc == 0 {
                        true
                    } else {
                        let err = io::Error::last_os_error();
                        if err.kind() == io::ErrorKind::AlreadyExists {
                            false
                        } else {
                            return Err(StorageBackendError::io(
                                "create capture queue directory",
                                err,
                            ));
                        }
                    };

                    // Always pin the directory descriptor with `O_NOFOLLOW` and
                    // verify `S_ISDIR` + `st_uid == expected_uid` via `fstat`
                    // strictly BEFORE any mode change (`fchmod` on the pinned fd),
                    // eliminating name-based chmod TOCTOU races on all Unix OSes.
                    let (fd, st) =
                        match openat_dir_nofollow(parent_fd, dir_name, expected_uid, subject) {
                            Ok(pair) => pair,
                            #[cfg(target_os = "linux")]
                            Err(StorageBackendError::Io {
                                kind: io::ErrorKind::PermissionDenied,
                                ..
                            }) if created_now => {
                                fix_newly_created_unreadable_dir_linux(
                                    parent_fd,
                                    dir_name,
                                    expected_uid,
                                    subject,
                                )?;
                                openat_dir_nofollow(parent_fd, dir_name, expected_uid, subject)?
                            }
                            Err(err) => return Err(err),
                        };
                    (fd, st, created_now)
                }
                Err(err) => return Err(err),
            };

        // `dir_fd` is already descriptor-pinned, verified non-symlink, `S_ISDIR`,
        // and owned by `expected_uid`. Enforce exact `0700` strictly via `fchmod(fd)`.
        if unix_perm_bits(st.st_mode) != QUEUE_DIR_MODE {
            fchmod_fd(
                &dir_fd,
                QUEUE_DIR_MODE,
                "enforce 0700 mode on capture queue directory",
            )?;
            let verified = fstat_fd(&dir_fd, "verify capture queue directory mode")?;
            if unix_perm_bits(verified.st_mode) != QUEUE_DIR_MODE {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} permissions failed to settle at 0700"),
                });
            }
        }
        if created_now {
            fsync_fd(parent_fd, "fsync capture queue parent directory")?;
        }
        Ok(dir_fd)
    }

    /// Exclusive advisory lock on `<queue_root>/append.lock` held across
    /// empty-lane classification, recovery, segment append, and dual-replica
    /// publication so concurrent initializers or writers never interleave
    /// (`ADR-RB-05` #1 & #3).
    pub(super) struct AppendLockGuard {
        _fd: OwnedFd,
    }

    impl AppendLockGuard {
        pub(super) fn from_fd(fd: OwnedFd) -> Self {
            Self { _fd: fd }
        }
    }

    pub(super) fn acquire_append_lock_at(
        root_fd: &OwnedFd,
        expected_uid: u32,
    ) -> StorageResult<AppendLockGuard> {
        acquire_named_lock_at(
            root_fd,
            QUEUE_APPEND_LOCK_FILE,
            expected_uid,
            "capture queue append.lock",
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MetaEntryState {
        Absent,
        RegularOwned,
    }

    fn inspect_meta_entry_nofollow(
        lane_fd: &OwnedFd,
        file_name: &str,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<MetaEntryState> {
        let cname = osstr_to_cstring(OsStr::new(file_name), "inspect capture queue metadata")?;
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
                return Ok(MetaEntryState::Absent);
            }
            return Err(StorageBackendError::io(
                "inspect capture queue metadata",
                err,
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let st = unsafe { st.assume_init() };
        if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
            return Err(map_queue_symlink_err(subject));
        }
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a regular file"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        Ok(MetaEntryState::RegularOwned)
    }

    fn count_segment_logs_in_dir_fd(lane_fd: &OwnedFd) -> StorageResult<usize> {
        // Duplicate the pinned lane directory fd with `FD_CLOEXEC` and iterate
        // entries directly from the open descriptor (zero path TOCTOU).
        // SAFETY: `lane_fd` is a valid open descriptor.
        let dup_raw = unsafe { libc::fcntl(lane_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_raw < 0 {
            return Err(StorageBackendError::io(
                "duplicate capture queue lane descriptor",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `dup_raw` is a valid directory descriptor; on success `fdopendir`
        // assumes ownership and `DirStreamGuard::drop` calls `closedir`.
        let dir_ptr = unsafe { libc::fdopendir(dup_raw) };
        if dir_ptr.is_null() {
            let err = io::Error::last_os_error();
            // SAFETY: `fdopendir` failed, so we still own `dup_raw`.
            unsafe {
                libc::close(dup_raw);
            }
            return Err(StorageBackendError::io(
                "open capture queue lane directory stream",
                err,
            ));
        }
        let guard = DirStreamGuard { ptr: dir_ptr };
        let mut count = 0usize;
        loop {
            clear_errno();
            // SAFETY: `guard.ptr` is a valid open `DIR*` stream.
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
            if matches_segment_log_glob_bytes(name_bytes) {
                count += 1;
            }
        }
        Ok(count)
    }

    fn write_all_fd(fd: &OwnedFd, data: &[u8], context: &'static str) -> StorageResult<()> {
        let mut written = 0usize;
        while written < data.len() {
            let slice = &data[written..];
            // SAFETY: `fd` is valid and `slice` points to readable bytes of length `slice.len()`.
            let rc = unsafe {
                libc::write(
                    fd.as_raw_fd(),
                    slice.as_ptr().cast::<libc::c_void>(),
                    slice.len(),
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(StorageBackendError::io(context, err));
            }
            if rc == 0 {
                return Err(StorageBackendError::io(
                    context,
                    io::Error::new(
                        io::ErrorKind::WriteZero,
                        "short write on capture queue meta",
                    ),
                ));
            }
            written += rc as usize;
        }
        Ok(())
    }

    /// Atomically create a `0600` initial `LTM2` metadata file inside `lane_fd`
    /// while holding exclusive `append.lock`, without ever following symlinks or
    /// clobbering any existing file:
    /// `openat(tmp, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600)`
    /// -> `fchmod(0600)` + verify -> `write_all(34B)` -> `fsync(tmp_fd)`
    /// -> `linkat(lane_fd, tmp, lane_fd, target, 0)` (no-clobber; fails closed on EEXIST)
    /// -> `unlinkat(lane_fd, tmp)` -> `fsync(lane_fd)`.
    fn publish_initial_meta_file_at(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        target_name: &'static str,
        bytes: &[u8],
        observer: &mut QueueInitObserver,
    ) -> StorageResult<()> {
        let tmp_name = next_meta_temp_name(target_name);
        let ctmp = osstr_to_cstring(OsStr::new(&tmp_name), "create capture queue temp meta")?;
        let ctarget = osstr_to_cstring(OsStr::new(target_name), "publish capture queue meta")?;

        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `lane_fd` is valid and `ctmp` is NUL-terminated.
        let raw_fd = unsafe {
            libc::openat(
                lane_fd.as_raw_fd(),
                ctmp.as_ptr(),
                flags,
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
            "enforce 0600 mode on capture queue metadata",
        )?;
        let st = fstat_fd(&tmp_fd, "verify capture queue metadata mode")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue temp metadata is not a regular file".to_string(),
            });
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue metadata permissions failed to settle at 0600".to_string(),
            });
        }

        write_all_fd(&tmp_fd, bytes, "write initial LTM2 metadata")?;
        fsync_fd(&tmp_fd, "sync initial LTM2 metadata")?;
        drop(tmp_fd);

        // Atomic no-clobber link into the pinned `lane_fd` directory while holding
        // `append.lock`. If an uncoordinated external actor created `target_name`
        // underneath us, fail closed immediately rather than publishing a mismatched
        // backup beside it.
        // SAFETY: `lane_fd` is valid and both `ctmp` and `ctarget` are NUL-terminated.
        let link_rc = unsafe {
            libc::linkat(
                lane_fd.as_raw_fd(),
                ctmp.as_ptr(),
                lane_fd.as_raw_fd(),
                ctarget.as_ptr(),
                0,
            )
        };
        if link_rc != 0 {
            let err = io::Error::last_os_error();
            return Err(StorageBackendError::io(
                "link initial LTM2 metadata into lane directory",
                err,
            ));
        }

        // Remove the staging link immediately and disarm guard.
        // SAFETY: `lane_fd` is valid and `ctmp` is NUL-terminated.
        let unlink_rc = unsafe { libc::unlinkat(lane_fd.as_raw_fd(), ctmp.as_ptr(), 0) };
        unlink_guard.disarm();
        if unlink_rc != 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "unlink initial LTM2 staging metadata",
                    err,
                ));
            }
        }

        observer.record_publish(lane_label, target_name);
        observer.before_fsync_lane(lane_label)?;
        fsync_fd(lane_fd, "fsync capture queue lane directory")?;
        Ok(())
    }

    fn init_lane_fd_under_lock(
        lane_fd: &OwnedFd,
        lane_label: &'static str,
        expected_uid: u32,
        observer: &mut QueueInitObserver,
        _lock: &AppendLockGuard,
    ) -> StorageResult<LaneInitOutcome> {
        let primary_state = inspect_meta_entry_nofollow(
            lane_fd,
            QUEUE_TAIL_META_FILE,
            expected_uid,
            "capture queue tail.meta",
        )?;
        let backup_state = inspect_meta_entry_nofollow(
            lane_fd,
            QUEUE_TAIL_META_BAK_FILE,
            expected_uid,
            "capture queue tail.meta.bak",
        )?;
        let seg_count = count_segment_logs_in_dir_fd(lane_fd)?;

        // ADR-RB-05 #3 & RB-07b AC4: Under exclusive `append.lock`, ONLY when
        // `events-*.log` count == 0 AND BOTH `tail.meta` and `tail.meta.bak`
        // are absent (`ENOENT`) may we pre-create the initial 34B `LTM2`
        // `(0, 0, 0, 0, [])` dual replicas + `fsync_dir`.
        if seg_count == 0
            && primary_state == MetaEntryState::Absent
            && backup_state == MetaEntryState::Absent
        {
            let initial_bytes = LaneTailMeta::initial_empty()
                .encode()
                .map_err(Ltm2CodecError::into_storage_error)?;
            debug_assert_eq!(initial_bytes.len(), LTM2_MIN_LEN);

            publish_initial_meta_file_at(
                lane_fd,
                lane_label,
                QUEUE_TAIL_META_FILE,
                &initial_bytes,
                observer,
            )?;
            publish_initial_meta_file_at(
                lane_fd,
                lane_label,
                QUEUE_TAIL_META_BAK_FILE,
                &initial_bytes,
                observer,
            )?;
            Ok(LaneInitOutcome::InitializedFreshEmpty)
        } else {
            Ok(LaneInitOutcome::PreservedExisting)
        }
    }

    fn init_children_under_root_fd(
        root_fd: &OwnedFd,
        paths: CaptureQueuePaths,
        owners: UnixExpectedQueueOwners,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<CaptureQueueInitReport> {
        let priority_fd = ensure_subdir_nofollow(
            root_fd,
            OsStr::new(QUEUE_LANE_PRIORITY_DIR),
            owners.priority_uid,
            "capture queue priority lane directory",
        )?;
        let normal_fd = ensure_subdir_nofollow(
            root_fd,
            OsStr::new(QUEUE_LANE_NORMAL_DIR),
            owners.normal_uid,
            "capture queue normal lane directory",
        )?;
        let _quarantine_fd = ensure_subdir_nofollow(
            root_fd,
            OsStr::new(QUEUE_QUARANTINE_DIR),
            owners.quarantine_uid,
            "capture queue quarantine directory",
        )?;

        // Acquire the shared cross-lane `append.lock` ONLY after every directory
        // passes `O_NOFOLLOW`/UID/`0700` verification, and hold it exclusively
        // across freshness classification and both initial replica publications
        // (`ADR-RB-05` #1 & #3).
        let append_lock = acquire_append_lock_at(root_fd, owners.root_uid)?;

        let priority_outcome = init_lane_fd_under_lock(
            &priority_fd,
            QUEUE_LANE_PRIORITY_DIR,
            owners.priority_uid,
            observer,
            &append_lock,
        )?;
        let normal_outcome = init_lane_fd_under_lock(
            &normal_fd,
            QUEUE_LANE_NORMAL_DIR,
            owners.normal_uid,
            observer,
            &append_lock,
        )?;

        drop(append_lock);

        Ok(CaptureQueueInitReport {
            paths,
            priority_outcome,
            normal_outcome,
        })
    }

    pub(super) fn init_capture_queue_unix(
        storage_dir: &Path,
        owners: UnixExpectedQueueOwners,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<CaptureQueueInitReport> {
        let storage_fd = open_ancestor_dir(storage_dir, "open repository storage directory")?;
        let private_fd = ensure_subdir_nofollow(
            &storage_fd,
            OsStr::new(PRIVATE_SUBDIR),
            owners.private_uid,
            "capture private directory",
        )?;
        let root_fd = ensure_subdir_nofollow(
            &private_fd,
            OsStr::new(CAPTURE_QUEUE_SUBDIR),
            owners.root_uid,
            "capture queue root directory",
        )?;
        init_children_under_root_fd(
            &root_fd,
            CaptureQueuePaths::from_storage_dir(storage_dir),
            owners,
            observer,
        )
    }

    pub(super) fn init_capture_queue_at_root_unix(
        queue_root: &Path,
        owners: UnixExpectedQueueOwners,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<CaptureQueueInitReport> {
        let root_fd =
            match open_dir_nofollow(queue_root, owners.root_uid, "capture queue root directory") {
                Ok((fd, st)) => {
                    if unix_perm_bits(st.st_mode) != QUEUE_DIR_MODE {
                        fchmod_fd(
                            &fd,
                            QUEUE_DIR_MODE,
                            "enforce 0700 mode on capture queue root directory",
                        )?;
                        let verified = fstat_fd(&fd, "verify capture queue root directory mode")?;
                        if unix_perm_bits(verified.st_mode) != QUEUE_DIR_MODE {
                            return Err(StorageBackendError::BackendInternal {
                            detail:
                                "capture queue root directory permissions failed to settle at 0700"
                                    .to_string(),
                        });
                        }
                    }
                    fd
                }
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {
                    let root_name: OsString = queue_root
                        .file_name()
                        .ok_or_else(|| StorageBackendError::BackendInternal {
                            detail: "capture queue root path has no final component".to_string(),
                        })?
                        .to_os_string();
                    let parent = queue_root
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."));
                    let parent_fd =
                        open_ancestor_dir(parent, "open capture queue parent directory")?;
                    ensure_subdir_nofollow(
                        &parent_fd,
                        root_name.as_os_str(),
                        owners.root_uid,
                        "capture queue root directory",
                    )?
                }
                Err(err) => return Err(err),
            };
        init_children_under_root_fd(
            &root_fd,
            CaptureQueuePaths::from_queue_root(queue_root),
            owners,
            observer,
        )
    }

    /// Acquire an exclusive `0600` `O_NOFOLLOW` lock file inside `dir_fd`
    /// using the macOS-safe two-step `openat(O_NOFOLLOW)` /
    /// `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` protocol + `flock(LOCK_EX)`.
    fn acquire_named_lock_at(
        dir_fd: &OwnedFd,
        file_name: &str,
        expected_uid: u32,
        subject: &'static str,
    ) -> StorageResult<AppendLockGuard> {
        let cname = osstr_to_cstring(OsStr::new(file_name), "open capture lock file")?;
        let open_existing_flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let create_excl_flags =
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let (lock_fd, created_now) = {
            let mut acquired = None;
            for _ in 0..32 {
                // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
                let raw_existing = unsafe {
                    libc::openat(dir_fd.as_raw_fd(), cname.as_ptr(), open_existing_flags)
                };
                if raw_existing >= 0 {
                    // SAFETY: `raw_existing >= 0` is exclusively owned here.
                    acquired = Some((unsafe { OwnedFd::from_raw_fd(raw_existing) }, false));
                    break;
                }
                let err = io::Error::last_os_error();
                if is_symlink_errno_at(&err, dir_fd, &cname) {
                    return Err(map_queue_symlink_err(subject));
                }
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(StorageBackendError::io("open capture lock file", err));
                }

                // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
                let raw_created = unsafe {
                    libc::openat(
                        dir_fd.as_raw_fd(),
                        cname.as_ptr(),
                        create_excl_flags,
                        QUEUE_FILE_MODE as libc::c_uint,
                    )
                };
                if raw_created >= 0 {
                    // SAFETY: `raw_created >= 0` is exclusively owned here.
                    acquired = Some((unsafe { OwnedFd::from_raw_fd(raw_created) }, true));
                    break;
                }
                let err2 = io::Error::last_os_error();
                if is_symlink_errno_at(&err2, dir_fd, &cname) {
                    return Err(map_queue_symlink_err(subject));
                }
                if err2.kind() == io::ErrorKind::AlreadyExists
                    || err2.kind() == io::ErrorKind::NotFound
                {
                    continue;
                }
                return Err(StorageBackendError::io("create capture lock file", err2));
            }
            acquired.ok_or_else(|| StorageBackendError::BackendInternal {
                detail: format!("{subject} open/create contended past retry bound"),
            })?
        };

        let st = fstat_fd(&lock_fd, "stat capture lock file")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} is not a regular file"),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(subject));
        }
        if unix_perm_bits(st.st_mode) != QUEUE_FILE_MODE {
            fchmod_fd(
                &lock_fd,
                QUEUE_FILE_MODE,
                "enforce 0600 mode on capture lock file",
            )?;
            let verified = fstat_fd(&lock_fd, "verify capture lock file mode")?;
            if unix_perm_bits(verified.st_mode) != QUEUE_FILE_MODE {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} permissions failed to settle at 0600"),
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
            return Err(StorageBackendError::io("lock capture lock file", err));
        }

        if created_now {
            fsync_fd(dir_fd, "fsync capture directory after creating lock file")?;
        }

        Ok(AppendLockGuard { _fd: lock_fd })
    }

    enum SpoolTombstoneSlotState {
        Absent,
        ValidExactMode,
        ValidNeedsTighten(OwnedFd),
    }

    pub(super) fn read_bounded_fd_with_context(
        fd: &OwnedFd,
        max_bytes: usize,
        context: &'static str,
    ) -> StorageResult<Vec<u8>> {
        let mut out = Vec::with_capacity(max_bytes.min(4096));
        let mut buf = [0u8; 4096];
        while out.len() < max_bytes {
            let want = (max_bytes - out.len()).min(buf.len());
            // SAFETY: `fd` is valid and `buf[..want]` is writable.
            let rc = unsafe {
                libc::read(
                    fd.as_raw_fd(),
                    buf.as_mut_ptr().cast::<libc::c_void>(),
                    want,
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(StorageBackendError::io(context, err));
            }
            if rc == 0 {
                break;
            }
            out.extend_from_slice(&buf[..rc as usize]);
        }
        Ok(out)
    }

    fn read_bounded_fd(fd: &OwnedFd, max_bytes: usize) -> StorageResult<Vec<u8>> {
        read_bounded_fd_with_context(fd, max_bytes, "read capture spool tombstone file")
    }

    fn inspect_spool_tombstone_slot_at(
        spool_fd: &OwnedFd,
        slot_name: &'static str,
        expected_uid: u32,
    ) -> StorageResult<SpoolTombstoneSlotState> {
        let cname = osstr_to_cstring(OsStr::new(slot_name), "inspect capture spool tombstone")?;
        // Pre-probe via `fstatat(AT_SYMLINK_NOFOLLOW)` to accurately distinguish
        // absent / symlink / directory / foreign-owned even on mode `0000` files,
        // then pin the file via `openat(O_RDONLY | O_NOFOLLOW)` and re-verify
        // `fstat(fd)` before reading or mutating permissions (zero TOCTOU).
        let mut pre_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `spool_fd` is valid and `cname` is NUL-terminated.
        let rc = unsafe {
            libc::fstatat(
                spool_fd.as_raw_fd(),
                cname.as_ptr(),
                pre_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(SpoolTombstoneSlotState::Absent);
            }
            return Err(StorageBackendError::io(
                "inspect capture spool tombstone",
                err,
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let pre_st = unsafe { pre_st.assume_init() };
        match pre_st.st_mode & libc::S_IFMT {
            libc::S_IFLNK => return Err(map_queue_symlink_err("capture spool tombstone")),
            libc::S_IFDIR => {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "live capture-spool subdirectory already exists; refusing to overwrite with tombstone"
                            .to_string(),
                });
            }
            libc::S_IFREG => {}
            _ => {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture spool tombstone path is not a regular file".to_string(),
                });
            }
        }
        if pre_st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture spool tombstone"));
        }

        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `spool_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(spool_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, spool_fd, &cname) {
                return Err(map_queue_symlink_err("capture spool tombstone"));
            }
            return Err(StorageBackendError::io(
                "open existing capture spool tombstone",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat existing capture spool tombstone")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture spool tombstone path is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture spool tombstone"));
        }
        let content = read_bounded_fd(&fd, CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len() + 1)?;
        if content.as_slice() != CAPTURE_SPOOL_TOMBSTONE_SENTINEL {
            return Err(StorageBackendError::BackendInternal {
                detail: "unexpected non-tombstone regular file at capture-spool path".to_string(),
            });
        }
        if unix_perm_bits(st.st_mode) == CAPTURE_SPOOL_TOMBSTONE_MODE {
            Ok(SpoolTombstoneSlotState::ValidExactMode)
        } else {
            Ok(SpoolTombstoneSlotState::ValidNeedsTighten(fd))
        }
    }

    /// Inode-pinned identity of a freshly published `0400` tombstone so
    /// mid-init failure rollback only ever unlinks the exact inode created by
    /// this call.
    struct PublishedTombstone {
        cname: CString,
        dev: libc::dev_t,
        ino: libc::ino_t,
    }

    fn unlink_if_same_inode(
        dir_fd: &OwnedFd,
        cname: &CString,
        expected_dev: libc::dev_t,
        expected_ino: libc::ino_t,
    ) {
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
            if st.st_dev == expected_dev && st.st_ino == expected_ino {
                // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
                unsafe {
                    libc::unlinkat(dir_fd.as_raw_fd(), cname.as_ptr(), 0);
                }
            }
        }
    }

    fn rollback_published_tombstones(spool_fd: &OwnedFd, published: &[PublishedTombstone]) {
        for item in published.iter().rev() {
            unlink_if_same_inode(spool_fd, &item.cname, item.dev, item.ino);
        }
        if !published.is_empty() {
            let _ = fsync_fd(spool_fd, "fsync capture spool directory after rollback");
        }
    }

    /// Best-effort cleanup guard for a private staging directory
    /// `.capture-spool.<pid>.<seq>.tmp` created under `private_fd` on fresh
    /// Redb initialization.
    struct StagingSpoolDirGuard<'a> {
        private_fd: &'a OwnedFd,
        dir_cname: CString,
        staging_fd: Option<OwnedFd>,
        active: bool,
    }

    impl<'a> StagingSpoolDirGuard<'a> {
        fn new(private_fd: &'a OwnedFd, dir_cname: CString, staging_fd: OwnedFd) -> Self {
            Self {
                private_fd,
                dir_cname,
                staging_fd: Some(staging_fd),
                active: true,
            }
        }

        fn staging_fd(&self) -> Option<&OwnedFd> {
            self.staging_fd.as_ref()
        }

        fn disarm(&mut self) -> Option<OwnedFd> {
            self.active = false;
            self.staging_fd.take()
        }
    }

    impl Drop for StagingSpoolDirGuard<'_> {
        fn drop(&mut self) {
            if !self.active {
                return;
            }
            if let Some(sfd) = self.staging_fd.take() {
                // Unlink every regular staging entry inside our private dir via
                // pinned `sfd` before `unlinkat(AT_REMOVEDIR)`.
                for entry_name in [
                    CAPTURE_SPOOL_CAPACITY_LOCK_FILE,
                    "tmp",
                    "ready",
                    "quarantine",
                ] {
                    if let Ok(c) = CString::new(entry_name) {
                        // SAFETY: `sfd` is valid and `c` is NUL-terminated.
                        unsafe {
                            libc::unlinkat(sfd.as_raw_fd(), c.as_ptr(), 0);
                        }
                    }
                }
                drop(sfd);
            }
            // SAFETY: `private_fd` is valid and `dir_cname` is NUL-terminated.
            unsafe {
                libc::unlinkat(
                    self.private_fd.as_raw_fd(),
                    self.dir_cname.as_ptr(),
                    libc::AT_REMOVEDIR,
                );
            }
        }
    }

    /// Descriptor-pinned `0400` tombstone publication (`RB-07e` AC3):
    /// Keeps `tmp_fd` OPEN across `linkat` (preventing inode recycling) and
    /// re-opens the published target with `O_NOFOLLOW` to verify its
    /// `(st_dev, st_ino)`, `S_IFREG`, `0400` mode, and `st_uid == expected_uid`
    /// match our pinned `tmp_fd` exactly before unlinking the staging name.
    fn publish_spool_tombstone_at(
        spool_fd: &OwnedFd,
        slot_name: &'static str,
        expected_uid: u32,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<PublishedTombstone> {
        let tmp_name = next_meta_temp_name(&format!(".{slot_name}.tombstone"));
        let ctmp = osstr_to_cstring(OsStr::new(&tmp_name), "create capture spool tombstone temp")?;
        let ctarget = osstr_to_cstring(OsStr::new(slot_name), "publish capture spool tombstone")?;

        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `spool_fd` is valid and `ctmp` is NUL-terminated.
        let raw_fd = unsafe {
            libc::openat(
                spool_fd.as_raw_fd(),
                ctmp.as_ptr(),
                flags,
                QUEUE_FILE_MODE as libc::c_uint,
            )
        };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, spool_fd, &ctmp) {
                return Err(map_queue_symlink_err("capture spool tombstone temp"));
            }
            return Err(StorageBackendError::io(
                "create capture spool tombstone temp",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here and stays OPEN across
        // `linkat` + post-publish `(st_dev, st_ino)` verification.
        let tmp_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut unlink_guard = TempUnlinkGuard::new(spool_fd, ctmp.clone());

        write_all_fd(
            &tmp_fd,
            CAPTURE_SPOOL_TOMBSTONE_SENTINEL,
            "write capture spool tombstone sentinel",
        )?;
        fchmod_fd(
            &tmp_fd,
            CAPTURE_SPOOL_TOMBSTONE_MODE,
            "enforce 0400 mode on capture spool tombstone",
        )?;
        let st = fstat_fd(&tmp_fd, "verify capture spool tombstone mode")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture spool tombstone temp is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err("capture spool tombstone"));
        }
        if unix_perm_bits(st.st_mode) != CAPTURE_SPOOL_TOMBSTONE_MODE {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture spool tombstone permissions failed to settle at 0400".to_string(),
            });
        }
        let expected_dev = st.st_dev;
        let expected_ino = st.st_ino;
        fsync_fd(&tmp_fd, "sync capture spool tombstone")?;

        // Atomic no-clobber link into `spool_fd` while `tmp_fd` remains open so
        // `(expected_dev, expected_ino)` cannot be recycled by the kernel.
        // SAFETY: `spool_fd` is valid and both `ctmp` and `ctarget` are NUL-terminated.
        let link_rc = unsafe {
            libc::linkat(
                spool_fd.as_raw_fd(),
                ctmp.as_ptr(),
                spool_fd.as_raw_fd(),
                ctarget.as_ptr(),
                0,
            )
        };
        if link_rc != 0 {
            return Err(StorageBackendError::io(
                "link capture spool tombstone into capture-spool directory",
                io::Error::last_os_error(),
            ));
        }

        // Verify the published target via `O_NOFOLLOW` matches the exact pinned
        // `(st_dev, st_ino)` of our still-open `tmp_fd` (defeats staging-name
        // swap TOCTOU before `linkat`).
        let verify_flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `spool_fd` is valid and `ctarget` is NUL-terminated.
        let pub_raw = unsafe { libc::openat(spool_fd.as_raw_fd(), ctarget.as_ptr(), verify_flags) };
        if pub_raw < 0 {
            let err = io::Error::last_os_error();
            unlink_if_same_inode(spool_fd, &ctarget, expected_dev, expected_ino);
            return Err(StorageBackendError::io(
                "verify published capture spool tombstone",
                err,
            ));
        }
        // SAFETY: `pub_raw >= 0` is exclusively owned here.
        let pub_fd = unsafe { OwnedFd::from_raw_fd(pub_raw) };
        let pub_st = fstat_fd(&pub_fd, "stat published capture spool tombstone")?;
        if pub_st.st_dev != expected_dev
            || pub_st.st_ino != expected_ino
            || (pub_st.st_mode & libc::S_IFMT) != libc::S_IFREG
            || pub_st.st_uid != expected_uid
            || unix_perm_bits(pub_st.st_mode) != CAPTURE_SPOOL_TOMBSTONE_MODE
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture spool tombstone inode identity mismatch after publish".to_string(),
            });
        }
        drop(pub_fd);

        // Unlink ONLY our exact staging inode and disarm guard while `tmp_fd` is still open.
        unlink_if_same_inode(spool_fd, &ctmp, expected_dev, expected_ino);
        unlink_guard.disarm();
        drop(tmp_fd);

        observer.record_publish(CAPTURE_SPOOL_SUBDIR, slot_name);
        Ok(PublishedTombstone {
            cname: ctarget,
            dev: expected_dev,
            ino: expected_ino,
        })
    }

    /// Stage a complete fresh `capture-spool/` directory (`0700`, `capacity.lock`
    /// `0600`, `{tmp,ready,quarantine}` `0400` tombstones, all synced) inside a
    /// hidden sibling `.capture-spool.<pid>.<seq>.tmp` and atomically publish it
    /// via `renameat` so a racing legacy `spool.rs:330` `ensure_spool` can never
    /// observe a half-initialized `capture-spool/` directory.
    ///
    /// Returns `Ok(Some(staging_fd))` if atomic publication succeeded, or
    /// `Ok(None)` if `capture-spool` already became non-empty concurrently
    /// (caller falls through to the locked existing-directory path).
    fn try_stage_fresh_spool_dir_atomic(
        private_fd: &OwnedFd,
        owners: UnixExpectedSpoolOwners,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<Option<OwnedFd>> {
        let staging_name = next_meta_temp_name(".capture-spool");
        let cstaging = osstr_to_cstring(
            OsStr::new(&staging_name),
            "create capture spool staging directory",
        )?;
        let cfinal = osstr_to_cstring(
            OsStr::new(CAPTURE_SPOOL_SUBDIR),
            "publish capture spool directory",
        )?;

        // SAFETY: `private_fd` is valid and `cstaging` is NUL-terminated.
        let mkdir_rc = unsafe {
            libc::mkdirat(
                private_fd.as_raw_fd(),
                cstaging.as_ptr(),
                QUEUE_DIR_MODE as libc::mode_t,
            )
        };
        if mkdir_rc != 0 {
            return Err(StorageBackendError::io(
                "create capture spool staging directory",
                io::Error::last_os_error(),
            ));
        }

        let (staging_fd, st) = match openat_dir_nofollow(
            private_fd,
            OsStr::new(&staging_name),
            owners.spool_uid,
            "capture spool staging directory",
        ) {
            Ok(pair) => pair,
            #[cfg(target_os = "linux")]
            Err(StorageBackendError::Io {
                kind: io::ErrorKind::PermissionDenied,
                ..
            }) => {
                fix_newly_created_unreadable_dir_linux(
                    private_fd,
                    OsStr::new(&staging_name),
                    owners.spool_uid,
                    "capture spool staging directory",
                )?;
                openat_dir_nofollow(
                    private_fd,
                    OsStr::new(&staging_name),
                    owners.spool_uid,
                    "capture spool staging directory",
                )?
            }
            Err(err) => {
                // SAFETY: best-effort remove empty staging dir on early failure.
                unsafe {
                    libc::unlinkat(
                        private_fd.as_raw_fd(),
                        cstaging.as_ptr(),
                        libc::AT_REMOVEDIR,
                    );
                }
                return Err(err);
            }
        };

        let mut staging_guard = StagingSpoolDirGuard::new(private_fd, cstaging.clone(), staging_fd);
        let Some(sfd) = staging_guard.staging_fd() else {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture spool staging descriptor missing".to_string(),
            });
        };

        if unix_perm_bits(st.st_mode) != QUEUE_DIR_MODE {
            fchmod_fd(
                sfd,
                QUEUE_DIR_MODE,
                "enforce 0700 mode on capture spool staging directory",
            )?;
            let verified = fstat_fd(sfd, "verify capture spool staging directory mode")?;
            if unix_perm_bits(verified.st_mode) != QUEUE_DIR_MODE {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture spool staging directory permissions failed to settle at 0700"
                        .to_string(),
                });
            }
        }

        // Create `capacity.lock` (`0600`) inside staging dir before publication.
        let _lock = acquire_named_lock_at(
            sfd,
            CAPTURE_SPOOL_CAPACITY_LOCK_FILE,
            owners.lock_uid,
            "capture spool capacity.lock",
        )?;

        // Populate all 3 `0400` tombstones inside the private staging directory.
        for &slot_name in &CAPTURE_SPOOL_TOMBSTONE_NAMES {
            let _ = publish_spool_tombstone_at(sfd, slot_name, owners.tombstone_uid, observer)?;
        }
        fsync_fd(sfd, "fsync capture spool staging directory")?;
        drop(_lock);

        // Atomically publish `.capture-spool.<pid>.<seq>.tmp` -> `capture-spool`.
        // POSIX guarantees `renameat(dirA, dirB)` fails with ENOTEMPTY/EEXIST if
        // `capture-spool` already exists and is non-empty (either another racer's
        // published tombstones or a live legacy spool with subdirs).
        // SAFETY: `private_fd` is valid and both CStrings are NUL-terminated.
        let rename_rc = unsafe {
            libc::renameat(
                private_fd.as_raw_fd(),
                cstaging.as_ptr(),
                private_fd.as_raw_fd(),
                cfinal.as_ptr(),
            )
        };
        if rename_rc != 0 {
            let err = io::Error::last_os_error();
            let raw = err.raw_os_error();
            if err.kind() == io::ErrorKind::AlreadyExists
                || raw == Some(libc::ENOTEMPTY)
                || raw == Some(libc::EEXIST)
            {
                // Lost the fresh-creation race: `staging_guard` cleans up our
                // staging dir cleanly and caller proceeds to the existing-dir path.
                return Ok(None);
            }
            return Err(StorageBackendError::io(
                "atomically publish capture-spool directory",
                err,
            ));
        }

        let published_fd = staging_guard.disarm();
        Ok(published_fd)
    }

    pub(super) fn init_redb_capture_spool_tombstones_unix(
        storage_dir: &Path,
        owners: UnixExpectedSpoolOwners,
        observer: &mut QueueInitObserver,
    ) -> StorageResult<CaptureSpoolTombstoneReport> {
        let storage_fd = open_ancestor_dir(storage_dir, "open repository storage directory")?;
        let private_fd = ensure_subdir_nofollow(
            &storage_fd,
            OsStr::new(PRIVATE_SUBDIR),
            owners.private_uid,
            "capture private directory",
        )?;

        // Fast-path fresh Redb repository: if `capture-spool` does not yet exist,
        // stage the entire `0700` directory + `0600` `capacity.lock` + all three
        // `0400` tombstones privately and publish atomically via `renameat` so
        // legacy `spool.rs:330` `ensure_spool` never sees a partial window.
        let spool_fd = match openat_dir_nofollow(
            &private_fd,
            OsStr::new(CAPTURE_SPOOL_SUBDIR),
            owners.spool_uid,
            "capture spool directory",
        ) {
            Ok((fd, st)) => {
                if unix_perm_bits(st.st_mode) != QUEUE_DIR_MODE {
                    fchmod_fd(
                        &fd,
                        QUEUE_DIR_MODE,
                        "enforce 0700 mode on capture spool directory",
                    )?;
                    let verified = fstat_fd(&fd, "verify capture spool directory mode")?;
                    if unix_perm_bits(verified.st_mode) != QUEUE_DIR_MODE {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "capture spool directory permissions failed to settle at 0700"
                                .to_string(),
                        });
                    }
                }
                fd
            }
            Err(StorageBackendError::Io {
                kind: io::ErrorKind::NotFound,
                ..
            }) => {
                if let Some(published_fd) =
                    try_stage_fresh_spool_dir_atomic(&private_fd, owners, observer)?
                {
                    observer.before_fsync_lane(PRIVATE_SUBDIR)?;
                    fsync_fd(&private_fd, "fsync capture private directory")?;
                    observer.before_fsync_lane(CAPTURE_SPOOL_SUBDIR)?;
                    fsync_fd(&published_fd, "fsync capture spool directory")?;
                    return Ok(CaptureSpoolTombstoneReport {
                        paths: CaptureSpoolTombstonePaths::from_storage_dir(storage_dir),
                        outcome: SpoolTombstoneOutcome::InstalledFresh,
                    });
                }
                // Another initializer or legacy hook populated `capture-spool/`
                // concurrently; validate and handle under `capacity.lock`.
                ensure_subdir_nofollow(
                    &private_fd,
                    OsStr::new(CAPTURE_SPOOL_SUBDIR),
                    owners.spool_uid,
                    "capture spool directory",
                )?
            }
            Err(err) => return Err(err),
        };

        // Existing `capture-spool/` path: serialize with concurrent initializers
        // and legacy `spool.rs` via `capture-spool/capacity.lock` (`0600`,
        // `O_NOFOLLOW`, `flock(LOCK_EX)`).
        let _capacity_lock = acquire_named_lock_at(
            &spool_fd,
            CAPTURE_SPOOL_CAPACITY_LOCK_FILE,
            owners.lock_uid,
            "capture spool capacity.lock",
        )?;

        // Pre-validate ALL 3 slots read-only BEFORE installing any tombstone so
        // an existing live legacy directory or symlink fails closed with zero
        // partial tombstone installation.
        let mut slots = Vec::with_capacity(CAPTURE_SPOOL_TOMBSTONE_NAMES.len());
        for &slot_name in &CAPTURE_SPOOL_TOMBSTONE_NAMES {
            let state =
                inspect_spool_tombstone_slot_at(&spool_fd, slot_name, owners.tombstone_uid)?;
            slots.push((slot_name, state));
        }

        let mut installed_any = false;
        let mut published_this_call: Vec<PublishedTombstone> = Vec::new();
        for (slot_name, state) in slots {
            match state {
                SpoolTombstoneSlotState::Absent => {
                    match publish_spool_tombstone_at(
                        &spool_fd,
                        slot_name,
                        owners.tombstone_uid,
                        observer,
                    ) {
                        Ok(pub_item) => {
                            published_this_call.push(pub_item);
                            installed_any = true;
                        }
                        Err(err) => {
                            // If an uncoordinated legacy `ensure_spool` raced in
                            // between slot publications and created a live subdir,
                            // roll back ONLY the exact tombstone inodes published
                            // by this call so no partial tombstones remain beside
                            // a live legacy spool directory.
                            rollback_published_tombstones(&spool_fd, &published_this_call);
                            return Err(err);
                        }
                    }
                }
                SpoolTombstoneSlotState::ValidNeedsTighten(fd) => {
                    if let Err(err) = (|| -> StorageResult<()> {
                        fchmod_fd(
                            &fd,
                            CAPTURE_SPOOL_TOMBSTONE_MODE,
                            "tighten capture spool tombstone mode to 0400",
                        )?;
                        let verified =
                            fstat_fd(&fd, "verify tightened capture spool tombstone mode")?;
                        if unix_perm_bits(verified.st_mode) != CAPTURE_SPOOL_TOMBSTONE_MODE {
                            return Err(StorageBackendError::BackendInternal {
                                detail:
                                    "capture spool tombstone permissions failed to settle at 0400"
                                        .to_string(),
                            });
                        }
                        fsync_fd(&fd, "sync tightened capture spool tombstone")?;
                        Ok(())
                    })() {
                        rollback_published_tombstones(&spool_fd, &published_this_call);
                        return Err(err);
                    }
                }
                SpoolTombstoneSlotState::ValidExactMode => {}
            }
        }

        // Unconditionally fsync BOTH parent (`private/`) and `capture-spool/`
        // before EVERY successful return (including `AlreadyFenced` retries
        // after a prior failed sync and concurrent initializers racing behind
        // an atomic directory publish) so success always guarantees durable
        // parent and child directory entries (`RB-07e` AC4).
        observer.before_fsync_lane(PRIVATE_SUBDIR)?;
        fsync_fd(&private_fd, "fsync capture private directory")?;
        observer.before_fsync_lane(CAPTURE_SPOOL_SUBDIR)?;
        fsync_fd(&spool_fd, "fsync capture spool directory")?;

        Ok(CaptureSpoolTombstoneReport {
            paths: CaptureSpoolTombstonePaths::from_storage_dir(storage_dir),
            outcome: if installed_any {
                SpoolTombstoneOutcome::InstalledFresh
            } else {
                SpoolTombstoneOutcome::AlreadyFenced
            },
        })
    }
}

#[cfg(unix)]
use unix_impl::{
    init_capture_queue_at_root_unix, init_capture_queue_unix,
    init_redb_capture_spool_tombstones_unix,
};

// ============================================================================
// Non-Unix Fallback Implementation
// ============================================================================

#[cfg(not(unix))]
fn ensure_non_unix_dir(path: &Path, subject: &'static str) -> StorageResult<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(map_queue_symlink_err(subject));
            }
            if !meta.is_dir() {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} is not a directory"),
                });
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|e| StorageBackendError::io("create capture queue directory", e))?;
            Ok(())
        }
        Err(err) => Err(StorageBackendError::io(
            "inspect capture queue directory",
            err,
        )),
    }
}

/// Flush a Windows directory handle durably via `CreateFileW`
/// (`FILE_FLAG_BACKUP_SEMANTICS`) + `FlushFileBuffers`, failing closed if the
/// OS refuses or fails to flush directory metadata.
#[cfg(windows)]
pub(super) fn flush_windows_dir(dir: &Path) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FlushFileBuffers, OPEN_EXISTING,
        },
    };

    struct WinHandleGuard(HANDLE);
    impl Drop for WinHandleGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid Win32 handle opened by `CreateFileW`.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 path alive for `CreateFileW`.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(StorageBackendError::io(
            "open Windows directory handle for durable flush",
            std::io::Error::last_os_error(),
        ));
    }
    let guard = WinHandleGuard(handle);
    // SAFETY: `guard.0` is a valid open directory handle.
    let ok = unsafe { FlushFileBuffers(guard.0) };
    if ok == 0 {
        return Err(StorageBackendError::io(
            "flush Windows directory buffers durably",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn enforce_windows_readonly(path: &Path, context: &'static str) -> StorageResult<()> {
    let mut perms = fs::metadata(path)
        .map_err(|e| StorageBackendError::io(context, e))?
        .permissions();
    if !perms.readonly() {
        perms.set_readonly(true);
        fs::set_permissions(path, perms).map_err(|e| StorageBackendError::io(context, e))?;
    }
    let verified = fs::metadata(path)
        .map_err(|e| StorageBackendError::io(context, e))?
        .permissions()
        .readonly();
    if !verified {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture spool tombstone readonly attribute failed to settle".to_string(),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn durable_no_clobber_publish_windows(tmp_path: &Path, target_path: &Path) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = target_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: Both `source` and `destination` are valid NUL-terminated UTF-16
    // buffers alive for the synchronous `MoveFileExW` call. Omitting
    // `MOVEFILE_REPLACE_EXISTING` enforces atomic no-clobber while
    // `MOVEFILE_WRITE_THROUGH` flushes the directory entry durably to disk.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(StorageBackendError::io(
            "durably publish initial LTM2 metadata with write-through rename",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn publish_initial_meta_non_unix(
    lane_dir: &Path,
    lane_label: &'static str,
    target_name: &'static str,
    bytes: &[u8],
    observer: &mut QueueInitObserver,
) -> StorageResult<()> {
    #[cfg(not(windows))]
    {
        let _ = (lane_dir, lane_label, target_name, bytes, observer);
        return Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue directory metadata sync is unsupported on this platform"
                .to_string(),
        });
    }
    #[cfg(windows)]
    {
        let tmp_path = lane_dir.join(next_meta_temp_name(target_name));
        let target_path = lane_dir.join(target_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| StorageBackendError::io("create capture queue temp metadata", e))?;
        if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(StorageBackendError::io("write initial LTM2 metadata", err));
        }
        drop(file);
        if let Err(err) = durable_no_clobber_publish_windows(&tmp_path, &target_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        observer.record_publish(lane_label, target_name);
        observer.before_fsync_lane(lane_label)?;
        flush_windows_dir(lane_dir)?;
        Ok(())
    }
}

#[cfg(windows)]
pub(super) fn durable_replace_publish_windows(
    tmp_path: &Path,
    target_path: &Path,
) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = target_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: Both `source` and `destination` are valid NUL-terminated UTF-16
    // buffers alive for the synchronous `MoveFileExW` call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(StorageBackendError::io(
            "durably replace capture queue metadata with write-through rename",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn acquire_append_lock_non_unix(queue_root: &Path) -> StorageResult<std::fs::File> {
    let lock_path = queue_root.join(QUEUE_APPEND_LOCK_FILE);
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        if meta.file_type().is_symlink() {
            return Err(map_queue_symlink_err("capture queue append.lock"));
        }
        if !meta.is_file() {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue append.lock is not a regular file".to_string(),
            });
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|e| StorageBackendError::io("open capture queue append.lock", e))?;
    file.lock()
        .map_err(|e| StorageBackendError::io("lock capture queue append.lock", e))?;
    Ok(file)
}

#[cfg(not(unix))]
fn init_lane_non_unix(
    lane_dir: &Path,
    lane_label: &'static str,
    observer: &mut QueueInitObserver,
    _lock: &std::fs::File,
) -> StorageResult<LaneInitOutcome> {
    let primary = tail_meta_path(lane_dir);
    let backup = tail_meta_bak_path(lane_dir);
    let check_absent = |p: &Path, subj: &'static str| -> StorageResult<bool> {
        match fs::symlink_metadata(p) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(map_queue_symlink_err(subj));
                }
                if !meta.is_file() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!("{subj} is not a regular file"),
                    });
                }
                Ok(false)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(err) => Err(StorageBackendError::io(
                "inspect capture queue metadata",
                err,
            )),
        }
    };
    let primary_absent = check_absent(&primary, "capture queue tail.meta")?;
    let backup_absent = check_absent(&backup, "capture queue tail.meta.bak")?;

    let mut seg_count = 0usize;
    for entry in fs::read_dir(lane_dir)
        .map_err(|e| StorageBackendError::io("read capture queue lane directory", e))?
    {
        let entry =
            entry.map_err(|e| StorageBackendError::io("read capture queue lane entry", e))?;
        if let Some(name) = entry.file_name().to_str() {
            if matches_segment_log_glob(name) {
                seg_count += 1;
            }
        } else {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue lane contains non-UTF-8 entry name".to_string(),
            });
        }
    }

    if seg_count == 0 && primary_absent && backup_absent {
        let initial_bytes = LaneTailMeta::initial_empty()
            .encode()
            .map_err(Ltm2CodecError::into_storage_error)?;
        publish_initial_meta_non_unix(
            lane_dir,
            lane_label,
            QUEUE_TAIL_META_FILE,
            &initial_bytes,
            observer,
        )?;
        publish_initial_meta_non_unix(
            lane_dir,
            lane_label,
            QUEUE_TAIL_META_BAK_FILE,
            &initial_bytes,
            observer,
        )?;
        Ok(LaneInitOutcome::InitializedFreshEmpty)
    } else {
        Ok(LaneInitOutcome::PreservedExisting)
    }
}

#[cfg(not(unix))]
fn init_capture_queue_non_unix(
    storage_dir: &Path,
    observer: &mut QueueInitObserver,
) -> StorageResult<CaptureQueueInitReport> {
    let private_dir = storage_dir.join(PRIVATE_SUBDIR);
    ensure_non_unix_dir(&private_dir, "capture private directory")?;
    init_capture_queue_at_root_non_unix(&private_dir.join(CAPTURE_QUEUE_SUBDIR), observer)
}

#[cfg(not(unix))]
fn init_capture_queue_at_root_non_unix(
    queue_root: &Path,
    observer: &mut QueueInitObserver,
) -> StorageResult<CaptureQueueInitReport> {
    #[cfg(not(windows))]
    {
        let _ = (queue_root, observer);
        return Err(StorageBackendError::BackendInternal {
            detail: "durable capture queue initialization is unsupported on this platform"
                .to_string(),
        });
    }
    #[cfg(windows)]
    {
        let paths = CaptureQueuePaths::from_queue_root(queue_root);
        ensure_non_unix_dir(&paths.root, "capture queue root directory")?;
        ensure_non_unix_dir(&paths.priority, "capture queue priority lane directory")?;
        ensure_non_unix_dir(&paths.normal, "capture queue normal lane directory")?;
        ensure_non_unix_dir(&paths.quarantine, "capture queue quarantine directory")?;
        let append_lock = acquire_append_lock_non_unix(&paths.root)?;
        let priority_outcome = init_lane_non_unix(
            &paths.priority,
            QUEUE_LANE_PRIORITY_DIR,
            observer,
            &append_lock,
        )?;
        let normal_outcome =
            init_lane_non_unix(&paths.normal, QUEUE_LANE_NORMAL_DIR, observer, &append_lock)?;
        drop(append_lock);
        Ok(CaptureQueueInitReport {
            paths,
            priority_outcome,
            normal_outcome,
        })
    }
}

#[cfg(windows)]
fn cleanup_windows_staging_spool_dir(staging_dir: &Path) {
    for name in [
        CAPTURE_SPOOL_CAPACITY_LOCK_FILE,
        "tmp",
        "ready",
        "quarantine",
    ] {
        let p = staging_dir.join(name);
        if let Ok(mut perms) = fs::metadata(&p).map(|m| m.permissions()) {
            perms.set_readonly(false);
            let _ = fs::set_permissions(&p, perms);
        }
        let _ = fs::remove_file(&p);
    }
    let _ = fs::remove_dir(staging_dir);
}

#[cfg(windows)]
fn try_stage_fresh_spool_dir_windows(
    private_dir: &Path,
    final_dir: &Path,
    observer: &mut QueueInitObserver,
) -> StorageResult<bool> {
    let staging_dir = private_dir.join(next_meta_temp_name(".capture-spool"));
    fs::create_dir(&staging_dir)
        .map_err(|e| StorageBackendError::io("create capture spool staging directory", e))?;

    let populate_res: StorageResult<()> = (|| {
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(staging_dir.join(CAPTURE_SPOOL_CAPACITY_LOCK_FILE))
            .map_err(|e| StorageBackendError::io("create capture spool capacity.lock", e))?;
        lock_file
            .sync_all()
            .map_err(|e| StorageBackendError::io("sync capture spool capacity.lock", e))?;
        drop(lock_file);

        for &slot_name in &CAPTURE_SPOOL_TOMBSTONE_NAMES {
            let slot_path = staging_dir.join(slot_name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&slot_path)
                .map_err(|e| StorageBackendError::io("create capture spool tombstone", e))?;
            file.write_all(CAPTURE_SPOOL_TOMBSTONE_SENTINEL)
                .and_then(|()| file.sync_all())
                .map_err(|e| {
                    StorageBackendError::io("write capture spool tombstone sentinel", e)
                })?;
            drop(file);
            enforce_windows_readonly(&slot_path, "mark capture spool tombstone readonly")?;
            observer.record_publish(CAPTURE_SPOOL_SUBDIR, slot_name);
        }
        Ok(())
    })();

    if let Err(err) = populate_res {
        cleanup_windows_staging_spool_dir(&staging_dir);
        return Err(err);
    }

    // Atomically publish the completely populated staging directory to
    // `capture-spool` via `MoveFileExW(MOVEFILE_WRITE_THROUGH)` without
    // `MOVEFILE_REPLACE_EXISTING`, preventing any legacy `ensure_spool` race
    // window on fresh Redb repositories.
    match durable_no_clobber_publish_windows(&staging_dir, final_dir) {
        Ok(()) => Ok(true),
        Err(StorageBackendError::Io {
            kind: std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied,
            ..
        }) if fs::symlink_metadata(final_dir).is_ok() => {
            cleanup_windows_staging_spool_dir(&staging_dir);
            Ok(false)
        }
        Err(err) => {
            cleanup_windows_staging_spool_dir(&staging_dir);
            Err(err)
        }
    }
}

#[cfg(not(unix))]
fn init_redb_capture_spool_tombstones_non_unix(
    storage_dir: &Path,
    observer: &mut QueueInitObserver,
) -> StorageResult<CaptureSpoolTombstoneReport> {
    #[cfg(not(windows))]
    {
        let _ = (storage_dir, observer);
        return Err(StorageBackendError::BackendInternal {
            detail:
                "durable capture-spool tombstone initialization is unsupported on this platform"
                    .to_string(),
        });
    }
    #[cfg(windows)]
    {
        let paths = CaptureSpoolTombstonePaths::from_storage_dir(storage_dir);
        let private_dir = storage_dir.join(PRIVATE_SUBDIR);
        ensure_non_unix_dir(&private_dir, "capture private directory")?;

        // Fast-path fresh Redb repository on Windows: atomically stage the
        // entire populated `capture-spool/` directory and publish with
        // `MOVEFILE_WRITE_THROUGH` so legacy hooks never see a partial window.
        match fs::symlink_metadata(&paths.root) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if try_stage_fresh_spool_dir_windows(&private_dir, &paths.root, observer)? {
                    observer.before_fsync_lane(PRIVATE_SUBDIR)?;
                    flush_windows_dir(&private_dir)?;
                    observer.before_fsync_lane(CAPTURE_SPOOL_SUBDIR)?;
                    flush_windows_dir(&paths.root)?;
                    return Ok(CaptureSpoolTombstoneReport {
                        paths,
                        outcome: SpoolTombstoneOutcome::InstalledFresh,
                    });
                }
            }
            Ok(_) => {}
            Err(err) => {
                return Err(StorageBackendError::io(
                    "inspect capture spool directory",
                    err,
                ));
            }
        }

        ensure_non_unix_dir(&paths.root, "capture spool directory")?;

        if let Ok(meta) = fs::symlink_metadata(&paths.capacity_lock) {
            if meta.file_type().is_symlink() {
                return Err(map_queue_symlink_err("capture spool capacity.lock"));
            }
            if !meta.is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture spool capacity.lock is not a regular file".to_string(),
                });
            }
        }
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&paths.capacity_lock)
            .map_err(|e| StorageBackendError::io("open capture spool capacity.lock", e))?;
        lock_file
            .lock()
            .map_err(|e| StorageBackendError::io("lock capture spool capacity.lock", e))?;

        let mut missing_slots = Vec::new();
        let mut needs_readonly_repair = Vec::new();
        for &slot_name in &CAPTURE_SPOOL_TOMBSTONE_NAMES {
            let slot_path = paths.root.join(slot_name);
            match fs::symlink_metadata(&slot_path) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(map_queue_symlink_err("capture spool tombstone"));
                    }
                    if meta.is_dir() {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "live capture-spool subdirectory already exists; refusing to overwrite with tombstone"
                                .to_string(),
                        });
                    }
                    if !meta.is_file() {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "capture spool tombstone path is not a regular file"
                                .to_string(),
                        });
                    }
                    if meta.len() != CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len() as u64 {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "unexpected non-tombstone regular file at capture-spool path"
                                .to_string(),
                        });
                    }
                    use std::io::Read as _;
                    let mut existing =
                        Vec::with_capacity(CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len() + 1);
                    OpenOptions::new()
                        .read(true)
                        .open(&slot_path)
                        .map_err(|e| {
                            StorageBackendError::io("open capture spool tombstone file", e)
                        })?
                        .take((CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len() + 1) as u64)
                        .read_to_end(&mut existing)
                        .map_err(|e| {
                            StorageBackendError::io("read capture spool tombstone file", e)
                        })?;
                    if existing.as_slice() != CAPTURE_SPOOL_TOMBSTONE_SENTINEL {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "unexpected non-tombstone regular file at capture-spool path"
                                .to_string(),
                        });
                    }
                    if !meta.permissions().readonly() {
                        needs_readonly_repair.push(slot_path);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    missing_slots.push((slot_name, slot_path));
                }
                Err(err) => {
                    return Err(StorageBackendError::io(
                        "inspect capture spool tombstone",
                        err,
                    ));
                }
            }
        }

        for slot_path in needs_readonly_repair {
            enforce_windows_readonly(&slot_path, "repair capture spool tombstone readonly")?;
        }

        let installed_any = !missing_slots.is_empty();
        let mut published_this_call: Vec<PathBuf> = Vec::new();
        for (slot_name, slot_path) in missing_slots {
            let tmp_path = paths
                .root
                .join(next_meta_temp_name(&format!(".{slot_name}.tombstone")));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)
            {
                Ok(f) => f,
                Err(e) => {
                    for p in published_this_call.iter().rev() {
                        if let Ok(mut perms) = fs::metadata(p).map(|m| m.permissions()) {
                            perms.set_readonly(false);
                            let _ = fs::set_permissions(p, perms);
                        }
                        let _ = fs::remove_file(p);
                    }
                    return Err(StorageBackendError::io(
                        "create capture spool tombstone temp",
                        e,
                    ));
                }
            };
            if let Err(err) = file
                .write_all(CAPTURE_SPOOL_TOMBSTONE_SENTINEL)
                .and_then(|()| file.sync_all())
            {
                drop(file);
                let _ = fs::remove_file(&tmp_path);
                for p in published_this_call.iter().rev() {
                    if let Ok(mut perms) = fs::metadata(p).map(|m| m.permissions()) {
                        perms.set_readonly(false);
                        let _ = fs::set_permissions(p, perms);
                    }
                    let _ = fs::remove_file(p);
                }
                return Err(StorageBackendError::io(
                    "write capture spool tombstone sentinel",
                    err,
                ));
            }
            drop(file);
            if let Err(err) =
                enforce_windows_readonly(&tmp_path, "mark capture spool tombstone readonly")
            {
                if let Ok(mut perms) = fs::metadata(&tmp_path).map(|m| m.permissions()) {
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(&tmp_path, perms);
                }
                let _ = fs::remove_file(&tmp_path);
                for p in published_this_call.iter().rev() {
                    if let Ok(mut perms) = fs::metadata(p).map(|m| m.permissions()) {
                        perms.set_readonly(false);
                        let _ = fs::set_permissions(p, perms);
                    }
                    let _ = fs::remove_file(p);
                }
                return Err(err);
            }
            if let Err(err) = durable_no_clobber_publish_windows(&tmp_path, &slot_path) {
                if let Ok(mut perms) = fs::metadata(&tmp_path).map(|m| m.permissions()) {
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(&tmp_path, perms);
                }
                let _ = fs::remove_file(&tmp_path);
                for p in published_this_call.iter().rev() {
                    if let Ok(mut perms) = fs::metadata(p).map(|m| m.permissions()) {
                        perms.set_readonly(false);
                        let _ = fs::set_permissions(p, perms);
                    }
                    let _ = fs::remove_file(p);
                }
                return Err(err);
            }
            published_this_call.push(slot_path);
            observer.record_publish(CAPTURE_SPOOL_SUBDIR, slot_name);
        }

        // Unconditionally flush BOTH parent (`private/`) and `capture-spool/`
        // durably via `FlushFileBuffers` before every `Ok` return (including
        // `AlreadyFenced` retries).
        observer.before_fsync_lane(PRIVATE_SUBDIR)?;
        flush_windows_dir(&private_dir)?;
        observer.before_fsync_lane(CAPTURE_SPOOL_SUBDIR)?;
        flush_windows_dir(&paths.root)?;
        drop(lock_file);

        Ok(CaptureSpoolTombstoneReport {
            paths,
            outcome: if installed_any {
                SpoolTombstoneOutcome::InstalledFresh
            } else {
                SpoolTombstoneOutcome::AlreadyFenced
            },
        })
    }
}

impl fmt::Display for LaneInitOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitializedFreshEmpty => f.write_str("initialized_fresh_empty"),
            Self::PreservedExisting => f.write_str("preserved_existing"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    /// `RB-07b` AC1–AC5:
    /// - AC1: Dual-lane initialization creates `priority/`, `normal/`, and
    ///   `quarantine/` with exact Unix `0700` permissions (and tightens owned
    ///   overly-permissive existing directories to exact `0700`).
    /// - AC2: Symlinked `private/`, queue root, `priority/`, `normal/`,
    ///   `quarantine/`, or `tail.meta*` are rejected fail-closed via `O_NOFOLLOW`
    ///   with zero writes into symlink targets.
    /// - AC3: Directories owned by a Unix UID different from the current
    ///   effective UID fail closed without modifying permissions or creating files.
    /// - AC4: Fresh empty lane initialization (`0` `events-*.log` and neither
    ///   `tail.meta` nor `tail.meta.bak` present) atomically creates `0600`
    ///   initial 34-byte `LTM2` `(0, 0, 0, 0, [])` dual replicas `tail.meta`
    ///   and `tail.meta.bak` before any segment file exists (while preserving
    ///   pre-existing segments/metadata untouched on non-fresh lanes).
    /// - AC5: Immediate `fsync_dir(lane_dir)` executes after publishing each
    ///   initial replica, and injected `fsync_dir` failure fails closed.
    #[test]
    fn dual_lane_directory_permissions_nofollow_owner_and_initial_meta() {
        // ------------------------------------------------------------------
        // AC1 + AC4 + AC5: Fresh dual-lane initialization on empty `.libra/`
        // ------------------------------------------------------------------
        let repo = tempdir().expect("create fresh repo storage dir");
        let mut observer = QueueInitObserver::new();
        #[cfg(unix)]
        let report = init_capture_queue_unix(
            repo.path(),
            UnixExpectedQueueOwners::current_effective(),
            &mut observer,
        )
        .expect("fresh dual-lane queue initialization must succeed");
        #[cfg(not(unix))]
        let report = init_capture_queue_non_unix(repo.path(), &mut observer)
            .expect("fresh dual-lane queue initialization must succeed");

        assert_eq!(
            report.priority_outcome,
            LaneInitOutcome::InitializedFreshEmpty
        );
        assert_eq!(
            report.normal_outcome,
            LaneInitOutcome::InitializedFreshEmpty
        );
        assert_eq!(
            report.paths.root,
            repo.path().join("private").join("capture-queue")
        );
        assert_eq!(
            report.paths.lane_dir(CaptureQueueLane::Priority),
            report.paths.priority.as_path()
        );
        assert_eq!(
            report.paths.lane_dir(CaptureQueueLane::Normal),
            report.paths.normal.as_path()
        );

        // AC1: Exact Unix `0700` on `private/`, `capture-queue/`, `priority/`, `normal/`, `quarantine/`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for dir in [
                repo.path().join("private"),
                report.paths.root.clone(),
                report.paths.priority.clone(),
                report.paths.normal.clone(),
                report.paths.quarantine.clone(),
            ] {
                let meta = fs::symlink_metadata(&dir).expect("stat queue directory");
                assert!(meta.is_dir() && !meta.file_type().is_symlink());
                assert_eq!(
                    meta.permissions().mode() & 0o7777,
                    0o700,
                    "AC1: directory must have exact 0700 mode"
                );
            }
        }

        // AC4: Verify ZERO segment files exist, ZERO `.tmp` files remain, `quarantine/`
        // has no metadata, and BOTH `priority/` and `normal/` hold exact 34B `0600`
        // `LTM2` `(0, 0, 0, 0, [])` dual replicas (`tail.meta` and `tail.meta.bak`).
        assert!(
            fs::read_dir(&report.paths.quarantine)
                .expect("read quarantine")
                .next()
                .is_none(),
            "quarantine directory must start empty with no tail.meta files"
        );

        let expected_initial_bytes = LaneTailMeta::initial_empty()
            .encode()
            .expect("encode initial empty LTM2");
        assert_eq!(
            expected_initial_bytes.len(),
            34,
            "AC4: initial LTM2 record must be exactly 34 bytes"
        );
        assert_eq!(&expected_initial_bytes[0..4], b"LTM2");
        assert_eq!(
            u16::from_le_bytes([expected_initial_bytes[4], expected_initial_bytes[5]]),
            2
        );
        assert_eq!(&expected_initial_bytes[6..30], &[0u8; 24]);
        assert_eq!(
            u32::from_le_bytes([
                expected_initial_bytes[30],
                expected_initial_bytes[31],
                expected_initial_bytes[32],
                expected_initial_bytes[33],
            ]),
            crc32c_checksum(&expected_initial_bytes[0..30])
        );

        for lane_dir in [&report.paths.priority, &report.paths.normal] {
            let mut entries: Vec<String> = fs::read_dir(lane_dir)
                .expect("read lane dir")
                .map(|e| {
                    e.expect("dir entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            entries.sort();
            assert_eq!(
                entries,
                vec!["tail.meta".to_string(), "tail.meta.bak".to_string()],
                "AC4: fresh lane must contain ONLY tail.meta and tail.meta.bak (zero events-*.log, zero .tmp)"
            );

            for meta_path in [tail_meta_path(lane_dir), tail_meta_bak_path(lane_dir)] {
                let fs_meta = fs::symlink_metadata(&meta_path).expect("stat tail meta");
                assert!(fs_meta.is_file() && !fs_meta.file_type().is_symlink());
                assert_eq!(fs_meta.len(), 34, "AC4: tail meta file must be 34 bytes");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    assert_eq!(
                        fs_meta.permissions().mode() & 0o7777,
                        0o600,
                        "AC4: tail meta file must have exact 0600 mode"
                    );
                }
                let raw = fs::read(&meta_path).expect("read tail meta");
                assert_eq!(raw, expected_initial_bytes);
                let decoded = LaneTailMeta::decode(&raw).expect("decode initial 34B LTM2");
                assert_eq!(decoded, LaneTailMeta::initial_empty());
                assert!(decoded.is_initial_empty());
                assert_eq!(decoded.ordering_key(), (0, 0, 0));
            }
        }

        // AC5: Exact ordered `publish -> fsync_dir(lane_dir)` trace for BOTH lanes!
        assert_eq!(
            observer.events,
            vec![
                QueueInitEvent::PublishInitialMeta {
                    lane: "priority",
                    file_name: "tail.meta",
                },
                QueueInitEvent::FsyncLaneDir { lane: "priority" },
                QueueInitEvent::PublishInitialMeta {
                    lane: "priority",
                    file_name: "tail.meta.bak",
                },
                QueueInitEvent::FsyncLaneDir { lane: "priority" },
                QueueInitEvent::PublishInitialMeta {
                    lane: "normal",
                    file_name: "tail.meta",
                },
                QueueInitEvent::FsyncLaneDir { lane: "normal" },
                QueueInitEvent::PublishInitialMeta {
                    lane: "normal",
                    file_name: "tail.meta.bak",
                },
                QueueInitEvent::FsyncLaneDir { lane: "normal" },
            ],
            "AC5: each initial replica publish must immediately execute fsync_dir(lane_dir)"
        );

        // AC5 fault-injection: if `fsync_dir(lane_dir)` fails, initialization fails closed (`LBR-IO-001`).
        {
            let fail_repo = tempdir().expect("create fsync fail repo");
            let mut fail_obs = QueueInitObserver::failing_fsync_at(0);
            #[cfg(unix)]
            let err = init_capture_queue_unix(
                fail_repo.path(),
                UnixExpectedQueueOwners::current_effective(),
                &mut fail_obs,
            )
            .expect_err("AC5: injected fsync_dir failure must fail closed");
            #[cfg(not(unix))]
            let err = init_capture_queue_non_unix(fail_repo.path(), &mut fail_obs)
                .expect_err("AC5: injected fsync_dir failure must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
        }

        // Idempotent re-initialization preserves existing dual replicas and tightens an
        // owned overly-permissive directory (`0o755` -> `0o700`) without touching metadata.
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&report.paths.priority, fs::Permissions::from_mode(0o755))
                    .expect("loosen owned priority dir to 0755");
            }
            let second = init_capture_queue(repo.path()).expect("idempotent re-init succeeds");
            assert_eq!(second.priority_outcome, LaneInitOutcome::PreservedExisting);
            assert_eq!(second.normal_outcome, LaneInitOutcome::PreservedExisting);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = fs::symlink_metadata(&report.paths.priority)
                    .expect("stat tightened priority")
                    .permissions()
                    .mode()
                    & 0o7777;
                assert_eq!(mode, 0o700, "owned overly-permissive dir tightened to 0700");
            }
        }

        // Non-fresh lanes MUST NEVER be reset or overwritten:
        // (a) Subcase B2 (`events-00000000.log` present with both `tail.meta*` absent) -> no meta synthesized.
        // (b) Case 0c (`0` segments with non-zero committed `tail.meta`) -> existing `tail.meta` preserved.
        {
            let non_fresh = tempdir().expect("create non-fresh queue root");
            let qroot = non_fresh.path().join("capture-queue");
            fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
            fs::create_dir_all(qroot.join("normal")).expect("mkdir normal");
            fs::create_dir_all(qroot.join("quarantine")).expect("mkdir quarantine");
            fs::write(qroot.join("priority").join("events-00000000.log"), b"seg0")
                .expect("seed existing segment in priority");

            let non_zero_meta = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 1,
                committed_len: 128,
                prefix_crc32c: 0xdead_beef,
                sealed: vec![SealedSegmentMeta {
                    seg_id: 0,
                    sealed_len: 4096,
                    sealed_crc32c: 0x1234_5678,
                }],
            };
            let non_zero_bytes = non_zero_meta.encode().expect("encode non-zero LTM2");
            assert_eq!(non_zero_bytes.len(), 34 + 16);
            fs::write(qroot.join("normal").join("tail.meta"), &non_zero_bytes)
                .expect("seed non-zero tail.meta in normal");

            let rep = init_capture_queue_at_root(&qroot)
                .expect("init on non-fresh lanes must succeed without mutating existing state");
            assert_eq!(rep.priority_outcome, LaneInitOutcome::PreservedExisting);
            assert_eq!(rep.normal_outcome, LaneInitOutcome::PreservedExisting);
            assert!(
                !tail_meta_path(&rep.paths.priority).exists()
                    && !tail_meta_bak_path(&rep.paths.priority).exists(),
                "Subcase B2 lane with existing events-*.log must NOT synthesize initial (0,0,0,0,[]) meta"
            );
            assert_eq!(
                fs::read(tail_meta_path(&rep.paths.normal)).expect("read preserved non-zero meta"),
                non_zero_bytes,
                "Case 0c non-zero tail.meta must never be reset to (0,0,0,0,[])"
            );
            assert!(
                !tail_meta_bak_path(&rep.paths.normal).exists(),
                "Case 0c missing backup must stay untouched during init"
            );
        }

        // ------------------------------------------------------------------
        // AC2: `O_NOFOLLOW` symlink rejection across every directory & meta path
        // ------------------------------------------------------------------
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt as _, symlink};

            let decoy = tempdir().expect("create decoy target dir");
            fs::set_permissions(decoy.path(), fs::Permissions::from_mode(0o700))
                .expect("set decoy 0700");

            // (2a) `.libra/private` is a symlink
            {
                let case = tempdir().expect("symlink private case");
                symlink(decoy.path(), case.path().join("private")).expect("symlink private");
                let err = init_capture_queue(case.path())
                    .expect_err("AC2: symlinked private/ must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err), "{err:?}");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert!(
                    !err.to_string().contains(&case.path().display().to_string()),
                    "sanitized error must not leak raw filesystem path"
                );
                assert!(
                    fs::read_dir(decoy.path())
                        .expect("decoy empty")
                        .next()
                        .is_none()
                );
            }

            // (2b) `.libra/private/capture-queue` (queue root) is a symlink (both via storage_dir & at_root)
            {
                let case = tempdir().expect("symlink root case");
                fs::create_dir(case.path().join("private")).expect("mkdir private");
                fs::set_permissions(
                    case.path().join("private"),
                    fs::Permissions::from_mode(0o700),
                )
                .expect("chmod private");
                let qroot = case.path().join("private").join("capture-queue");
                symlink(decoy.path(), &qroot).expect("symlink capture-queue");

                let err1 = init_capture_queue(case.path())
                    .expect_err("AC2: symlinked capture-queue/ must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err1), "{err1:?}");
                assert_eq!(err1.stable_code_str(), "LBR-IO-001");

                let err2 = init_capture_queue_at_root(&qroot)
                    .expect_err("AC2: direct symlinked queue_root must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err2), "{err2:?}");
                assert_eq!(err2.stable_code_str(), "LBR-IO-001");
                assert!(
                    fs::read_dir(decoy.path())
                        .expect("decoy empty")
                        .next()
                        .is_none()
                );
            }

            // (2c) Each child dir (`priority/`, `normal/`, `quarantine/`) as symlink (real & dangling)
            for child in ["priority", "normal", "quarantine"] {
                for dangling in [false, true] {
                    let case = tempdir().expect("symlink child case");
                    let qroot = case.path().join("private").join("capture-queue");
                    fs::create_dir_all(&qroot).expect("mkdir qroot");
                    fs::set_permissions(
                        case.path().join("private"),
                        fs::Permissions::from_mode(0o700),
                    )
                    .expect("chmod private");
                    fs::set_permissions(&qroot, fs::Permissions::from_mode(0o700))
                        .expect("chmod qroot");
                    let target = if dangling {
                        case.path().join("does-not-exist-target")
                    } else {
                        decoy.path().to_path_buf()
                    };
                    symlink(&target, qroot.join(child)).expect("symlink child");
                    let err = init_capture_queue(case.path()).expect_err(&format!(
                        "AC2: symlinked {child}/ (dangling={dangling}) must fail closed"
                    ));
                    assert!(
                        is_capture_queue_symlink_rejected_error(&err),
                        "{child} dangling={dangling}: {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-IO-001");
                    assert!(
                        fs::read_dir(decoy.path())
                            .expect("decoy empty")
                            .next()
                            .is_none()
                    );
                }
            }

            // (2d) Regular-file tombstone planted at `priority` -> rejected fail-closed as non-dir.
            {
                let case = tempdir().expect("file tombstone child case");
                let qroot = case.path().join("capture-queue");
                fs::create_dir(&qroot).expect("mkdir qroot");
                fs::set_permissions(&qroot, fs::Permissions::from_mode(0o700))
                    .expect("chmod qroot");
                fs::write(qroot.join("priority"), b"tombstone").expect("write regular file");
                let err = init_capture_queue_at_root(&qroot)
                    .expect_err("regular file at priority/ must fail closed");
                assert!(!is_capture_queue_symlink_rejected_error(&err));
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    fs::read(qroot.join("priority")).expect("tombstone untouched"),
                    b"tombstone"
                );
            }

            // (2e) Symlinked `tail.meta` or `tail.meta.bak` inside a lane -> rejected fail-closed!
            for meta_name in [QUEUE_TAIL_META_FILE, QUEUE_TAIL_META_BAK_FILE] {
                let case = tempdir().expect("symlink meta case");
                let qroot = case.path().join("capture-queue");
                fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
                fs::set_permissions(&qroot, fs::Permissions::from_mode(0o700))
                    .expect("chmod qroot");
                fs::set_permissions(qroot.join("priority"), fs::Permissions::from_mode(0o700))
                    .expect("chmod priority");
                symlink(
                    decoy.path().join("evil.bin"),
                    qroot.join("priority").join(meta_name),
                )
                .expect("symlink meta");
                let err = init_capture_queue_at_root(&qroot)
                    .expect_err("AC2: symlinked tail.meta* must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err), "{err:?}");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert!(!decoy.path().join("evil.bin").exists());
            }
        }

        // ------------------------------------------------------------------
        // AC3: Unix non-effective-UID owner mismatch rejection (zero mutations!)
        // ------------------------------------------------------------------
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let real_owners = UnixExpectedQueueOwners::current_effective();
            let foreign_uid = real_owners.root_uid.wrapping_add(1);
            assert_ne!(foreign_uid, real_owners.root_uid);

            let owner_cases: [(&str, UnixExpectedQueueOwners); 5] = [
                (
                    "private",
                    UnixExpectedQueueOwners {
                        private_uid: foreign_uid,
                        ..real_owners
                    },
                ),
                (
                    "capture-queue",
                    UnixExpectedQueueOwners {
                        root_uid: foreign_uid,
                        ..real_owners
                    },
                ),
                (
                    "priority",
                    UnixExpectedQueueOwners {
                        priority_uid: foreign_uid,
                        ..real_owners
                    },
                ),
                (
                    "normal",
                    UnixExpectedQueueOwners {
                        normal_uid: foreign_uid,
                        ..real_owners
                    },
                ),
                (
                    "quarantine",
                    UnixExpectedQueueOwners {
                        quarantine_uid: foreign_uid,
                        ..real_owners
                    },
                ),
            ];

            for (label, custom_owners) in owner_cases {
                let case = tempdir().expect("owner mismatch case");
                let qroot = case.path().join("private").join("capture-queue");
                fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
                fs::create_dir_all(qroot.join("normal")).expect("mkdir normal");
                fs::create_dir_all(qroot.join("quarantine")).expect("mkdir quarantine");
                // Deliberately set mode 0755 on the tested dir to prove we fail closed
                // on owner mismatch WITHOUT mutating permissions or creating files!
                let target_dir = match label {
                    "private" => case.path().join("private"),
                    "capture-queue" => qroot.clone(),
                    child => qroot.join(child),
                };
                fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o755))
                    .expect("set 0755 on foreign-owned test dir");

                let mut obs = QueueInitObserver::new();
                let err = init_capture_queue_unix(case.path(), custom_owners, &mut obs)
                    .expect_err(&format!("AC3 ({label}): non-owner UID must fail closed"));
                assert!(
                    is_capture_queue_owner_mismatch_error(&err),
                    "AC3 ({label}): expected owner mismatch error, got {err:?}"
                );
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                let err_text = err.to_string();
                assert!(
                    !err_text.contains(&foreign_uid.to_string())
                        && !err_text.contains(&case.path().display().to_string()),
                    "AC3 ({label}): error must not leak raw UID or path: {err_text}"
                );
                assert_eq!(
                    fs::symlink_metadata(&target_dir)
                        .expect("stat target dir")
                        .permissions()
                        .mode()
                        & 0o7777,
                    0o755,
                    "AC3 ({label}): foreign-owned directory permissions must NEVER be mutated"
                );
                assert!(
                    obs.events.is_empty(),
                    "AC3 ({label}): zero metadata files may be published on owner mismatch"
                );
                assert!(
                    !tail_meta_path(&qroot.join("priority")).exists()
                        && !tail_meta_path(&qroot.join("normal")).exists(),
                    "AC3 ({label}): zero tail.meta files may be created on owner mismatch"
                );
            }
        }

        // ------------------------------------------------------------------
        // LTM2 codec boundary & negative-invariant tests (0..=256 sealed entries)
        // ------------------------------------------------------------------
        {
            let full_sealed: Vec<SealedSegmentMeta> = (10..10 + MAX_SEALED_SEGMENTS as u32)
                .map(|seg_id| SealedSegmentMeta {
                    seg_id,
                    sealed_len: 1024 + u64::from(seg_id),
                    sealed_crc32c: 0xa000_0000 ^ seg_id,
                })
                .collect();
            let max_meta = LaneTailMeta {
                min_live_seg: 10,
                active_seg: 10 + MAX_SEALED_SEGMENTS as u32,
                committed_len: 65_536,
                prefix_crc32c: 0x0bad_c0de,
                sealed: full_sealed,
            };
            let encoded = max_meta.encode().expect("256 sealed entries must encode");
            assert_eq!(encoded.len(), LTM2_MAX_LEN);
            assert_eq!(
                LaneTailMeta::decode(&encoded).expect("256 sealed entries must decode"),
                max_meta
            );

            // Corrupt CRC32C rejected
            let mut bad_crc = encoded.clone();
            bad_crc[14] ^= 0x01;
            assert_eq!(
                LaneTailMeta::decode(&bad_crc),
                Err(Ltm2CodecError::CrcMismatch)
            );

            // Bad magic / version / length rejected
            let mut bad_magic = expected_initial_bytes.clone();
            bad_magic[3] = b'1';
            assert_eq!(
                LaneTailMeta::decode(&bad_magic),
                Err(Ltm2CodecError::MagicMismatch)
            );
            let mut bad_ver = expected_initial_bytes.clone();
            bad_ver[4] = 3;
            assert_eq!(
                LaneTailMeta::decode(&bad_ver),
                Err(Ltm2CodecError::UnsupportedVersion { version: 3 })
            );
            assert!(matches!(
                LaneTailMeta::decode(&expected_initial_bytes[..33]),
                Err(Ltm2CodecError::InvalidLength { .. })
            ));
            let mut extra_byte = expected_initial_bytes.clone();
            extra_byte.push(0);
            assert!(matches!(
                LaneTailMeta::decode(&extra_byte),
                Err(Ltm2CodecError::InvalidLength { .. })
            ));

            // 257 sealed entries rejected on encode
            let mut too_many = max_meta.clone();
            too_many.active_seg += 1;
            too_many.sealed.push(SealedSegmentMeta {
                seg_id: 10 + MAX_SEALED_SEGMENTS as u32,
                sealed_len: 100,
                sealed_crc32c: 1,
            });
            assert!(matches!(
                too_many.encode(),
                Err(Ltm2CodecError::InvalidInvariant { .. })
            ));

            // Non-contiguous seg_id rejected
            let mut bad_gap = LaneTailMeta {
                min_live_seg: 0,
                active_seg: 2,
                committed_len: 0,
                prefix_crc32c: 0,
                sealed: vec![
                    SealedSegmentMeta {
                        seg_id: 0,
                        sealed_len: 10,
                        sealed_crc32c: 1,
                    },
                    SealedSegmentMeta {
                        seg_id: 2, // skipped 1!
                        sealed_len: 10,
                        sealed_crc32c: 2,
                    },
                ],
            };
            assert!(matches!(
                bad_gap.encode(),
                Err(Ltm2CodecError::InvalidInvariant { .. })
            ));
            bad_gap.sealed[1].seg_id = 1;
            assert!(bad_gap.encode().is_ok());
            assert_eq!(
                Ltm2CodecError::CrcMismatch
                    .into_storage_error()
                    .stable_code_str(),
                "LBR-IO-001"
            );
        }

        // ------------------------------------------------------------------
        // Codex R1 P1 #1: Concurrent initializer serialization under shared
        // `append.lock` + blocked-writer interleaving + `append.lock` posture
        // ------------------------------------------------------------------
        {
            use std::sync::{Arc, Barrier};

            // (a) Verify `append.lock` created at 0600 on Unix and rejects symlinks fail-closed.
            #[cfg(unix)]
            {
                use std::os::unix::fs::{PermissionsExt as _, symlink};

                let lock_meta =
                    fs::symlink_metadata(report.paths.root.join(QUEUE_APPEND_LOCK_FILE))
                        .expect("stat append.lock");
                assert!(lock_meta.is_file() && !lock_meta.file_type().is_symlink());
                assert_eq!(
                    lock_meta.permissions().mode() & 0o7777,
                    0o600,
                    "append.lock must have exact 0600 mode"
                );

                let sym_case = tempdir().expect("symlink append.lock case");
                let qroot = sym_case.path().join("capture-queue");
                fs::create_dir(&qroot).expect("mkdir qroot");
                fs::set_permissions(&qroot, fs::Permissions::from_mode(0o700))
                    .expect("chmod qroot");
                symlink(
                    sym_case.path().join("decoy.lock"),
                    qroot.join(QUEUE_APPEND_LOCK_FILE),
                )
                .expect("symlink append.lock");
                let err = init_capture_queue_at_root(&qroot)
                    .expect_err("symlinked append.lock must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err), "{err:?}");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert!(!tail_meta_path(&qroot.join("priority")).exists());
            }

            // (b) 8 concurrent threads racing `init_capture_queue` on a brand-new repo:
            // exactly 1 initializes both lanes fresh and 7 observe `PreservedExisting`,
            // and EVERY returning thread immediately observes BOTH `tail.meta` and
            // `tail.meta.bak` complete on disk for both lanes!
            let race_repo = tempdir().expect("create concurrent init race repo");
            let race_path = Arc::new(race_repo.path().to_path_buf());
            let barrier = Arc::new(Barrier::new(8));
            let expected_bytes = Arc::new(expected_initial_bytes.clone());
            let mut handles = Vec::with_capacity(8);
            for _ in 0..8 {
                let p = Arc::clone(&race_path);
                let b = Arc::clone(&barrier);
                let exp = Arc::clone(&expected_bytes);
                handles.push(std::thread::spawn(move || {
                    b.wait();
                    let rep = init_capture_queue(&p).expect("concurrent init must succeed");
                    // Every returning thread must ALREADY see BOTH complete replicas on disk!
                    for lane_dir in [&rep.paths.priority, &rep.paths.normal] {
                        assert_eq!(
                            fs::read(tail_meta_path(lane_dir)).expect("read primary"),
                            *exp
                        );
                        assert_eq!(
                            fs::read(tail_meta_bak_path(lane_dir)).expect("read backup"),
                            *exp
                        );
                    }
                    (rep.priority_outcome, rep.normal_outcome)
                }));
            }
            let outcomes: Vec<(LaneInitOutcome, LaneInitOutcome)> = handles
                .into_iter()
                .map(|h| h.join().expect("race worker thread must not panic"))
                .collect();
            let fresh_count = outcomes
                .iter()
                .filter(|&&(p, n)| {
                    p == LaneInitOutcome::InitializedFreshEmpty
                        && n == LaneInitOutcome::InitializedFreshEmpty
                })
                .count();
            let preserved_count = outcomes
                .iter()
                .filter(|&&(p, n)| {
                    p == LaneInitOutcome::PreservedExisting
                        && n == LaneInitOutcome::PreservedExisting
                })
                .count();
            assert_eq!(
                fresh_count, 1,
                "exactly one racing initializer may perform fresh dual-replica publication: {outcomes:?}"
            );
            assert_eq!(
                preserved_count, 7,
                "all other racing initializers must serialize behind append.lock and preserve: {outcomes:?}"
            );

            // (c) Deterministic interleaving: writer holds `append.lock`, writes a live
            // non-empty `tail.meta` while a racing `init_capture_queue` blocks on `append.lock`.
            // Once lock is released, initializer observes the live primary and MUST NOT
            // publish an initial `(0,0,0,0,[])` `tail.meta.bak` beside it!
            #[cfg(unix)]
            {
                use std::sync::mpsc;

                let ilv_repo = tempdir().expect("create interleaving repo");
                let qroot = ilv_repo.path().join("private").join("capture-queue");
                fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
                fs::create_dir_all(qroot.join("normal")).expect("mkdir normal");
                fs::create_dir_all(qroot.join("quarantine")).expect("mkdir quarantine");
                let lock_file = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(qroot.join(QUEUE_APPEND_LOCK_FILE))
                    .expect("open append.lock");
                {
                    use std::os::unix::io::AsRawFd as _;
                    let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
                    assert_eq!(rc, 0, "acquire exclusive append.lock in writer");
                }

                let (tx, rx) = mpsc::channel();
                let ilv_path = ilv_repo.path().to_path_buf();
                let init_handle = std::thread::spawn(move || {
                    let rep = init_capture_queue(&ilv_path).expect("interleaved init must succeed");
                    let _ = tx.send((rep.priority_outcome, rep.normal_outcome));
                });

                // Confirm initializer is genuinely blocked behind `append.lock`.
                assert!(
                    rx.recv_timeout(std::time::Duration::from_millis(80))
                        .is_err(),
                    "init_capture_queue must block while another actor holds append.lock"
                );

                // While holding `append.lock`, writer publishes a live non-empty primary in `priority/`.
                let live_meta = LaneTailMeta {
                    min_live_seg: 0,
                    active_seg: 0,
                    committed_len: 512,
                    prefix_crc32c: 0xcafe_babe,
                    sealed: Vec::new(),
                };
                let live_bytes = live_meta.encode().expect("encode live LTM2");
                fs::write(tail_meta_path(&qroot.join("priority")), &live_bytes)
                    .expect("writer publishes live primary under append.lock");

                // Release `append.lock`; blocked initializer now runs cleanly.
                drop(lock_file);
                let (prio_out, norm_out) = rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("initializer unblocks after append.lock release");
                init_handle.join().expect("join initializer");

                assert_eq!(prio_out, LaneInitOutcome::PreservedExisting);
                assert_eq!(norm_out, LaneInitOutcome::InitializedFreshEmpty);
                assert_eq!(
                    fs::read(tail_meta_path(&qroot.join("priority"))).expect("read live primary"),
                    live_bytes,
                    "live primary written under append.lock must stay untouched"
                );
                assert!(
                    !tail_meta_bak_path(&qroot.join("priority")).exists(),
                    "initializer must NEVER pair an initial (0,0,0,0,[]) backup beside a live primary"
                );
                assert_eq!(
                    fs::read(tail_meta_path(&qroot.join("normal"))).expect("read normal primary"),
                    expected_initial_bytes
                );
                assert_eq!(
                    fs::read(tail_meta_bak_path(&qroot.join("normal")))
                        .expect("read normal backup"),
                    expected_initial_bytes
                );
            }
        }

        // ------------------------------------------------------------------
        // Codex R1 P1 #3 + R2 P1: Raw-byte `events-*.log` matching handles
        // non-UTF-8 Unix filenames (`events-\xff.log`) AND zero-length glob
        // suffixes (`events-.log`) without treating lane as fresh empty!
        // ------------------------------------------------------------------
        {
            assert!(matches_segment_log_glob_bytes(b"events-00000000.log"));
            assert!(matches_segment_log_glob_bytes(b"events-\xff.log"));
            assert!(matches_segment_log_glob_bytes(b"events-.log"));
            assert!(!matches_segment_log_glob_bytes(b"events-"));
            assert!(!matches_segment_log_glob_bytes(b"other-\xff.log"));

            // Filesystem regression for zero-length suffix `events-.log`
            {
                let empty_suffix_dir = tempdir().expect("create events-.log test dir");
                let qroot = empty_suffix_dir.path().join("capture-queue");
                fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
                fs::create_dir_all(qroot.join("normal")).expect("mkdir normal");
                fs::create_dir_all(qroot.join("quarantine")).expect("mkdir quarantine");
                fs::write(qroot.join("priority").join("events-.log"), b"seg")
                    .expect("write events-.log");
                let rep = init_capture_queue_at_root(&qroot)
                    .expect("init must preserve lane containing events-.log");
                assert_eq!(rep.priority_outcome, LaneInitOutcome::PreservedExisting);
                assert!(
                    !tail_meta_path(&rep.paths.priority).exists()
                        && !tail_meta_bak_path(&rep.paths.priority).exists(),
                    "lane containing events-.log must NEVER synthesize initial meta"
                );
                assert_eq!(rep.normal_outcome, LaneInitOutcome::InitializedFreshEmpty);
            }

            #[cfg(unix)]
            {
                let non_utf8_dir = tempdir().expect("create non-utf8 segment test dir");
                let qroot = non_utf8_dir.path().join("capture-queue");
                fs::create_dir_all(qroot.join("priority")).expect("mkdir priority");
                fs::create_dir_all(qroot.join("normal")).expect("mkdir normal");
                fs::create_dir_all(qroot.join("quarantine")).expect("mkdir quarantine");
                let bad_name = OsStr::from_bytes(b"events-\xff.log");
                // Some filesystems (e.g. Linux ext4/tmpfs) allow non-UTF-8 filenames while
                // macOS APFS rejects them at creation with EINVAL/EILSEQ. When creation
                // succeeds on the host filesystem, verify the lane is NOT initialized empty.
                if fs::write(qroot.join("priority").join(bad_name), b"non-utf8-seg").is_ok() {
                    let rep = init_capture_queue_at_root(&qroot)
                        .expect("init must preserve non-UTF-8 segment lane");
                    assert_eq!(rep.priority_outcome, LaneInitOutcome::PreservedExisting);
                    assert!(
                        !tail_meta_path(&rep.paths.priority).exists()
                            && !tail_meta_bak_path(&rep.paths.priority).exists(),
                        "lane containing events-\\xff.log must NEVER synthesize initial meta"
                    );
                }
            }
        }
    }

    /// `RB-07e` AC1–AC5:
    /// - AC1: `init_redb_capture_spool_tombstones` creates `.libra/private/capture-spool/`
    ///   with exact Unix `0700` permissions.
    /// - AC2: Creates `.libra/private/capture-spool/capacity.lock` with exact Unix `0600`
    ///   permissions.
    /// - AC3: Atomically creates `0400` regular-file tombstones at
    ///   `capture-spool/{tmp,ready,quarantine}`.
    /// - AC4: Executes `fsync_dir(capture-spool)` immediately after creating the tombstones
    ///   (and fails closed on injected `fsync_dir` failure).
    /// - AC5: Legacy SQLite hook `crate::internal::ai::capture::spool::publish` (which invokes
    ///   `spool.rs:330` `ensure_spool`) immediately fails closed on the tombstoned fresh Redb
    ///   repo without mutating or removing any tombstone.
    #[test]
    fn fresh_redb_init_spool_tombstones_blocks_legacy_ensure_spool() {
        // ------------------------------------------------------------------
        // AC1 + AC2 + AC3 + AC4: Fresh Redb repo tombstone initialization
        // ------------------------------------------------------------------
        let repo = tempdir().expect("create fresh redb repo dir");
        let mut observer = QueueInitObserver::new();
        #[cfg(unix)]
        let report = init_redb_capture_spool_tombstones_unix(
            repo.path(),
            UnixExpectedSpoolOwners::current_effective(),
            &mut observer,
        )
        .expect("fresh Redb capture-spool tombstone init must succeed");
        #[cfg(not(unix))]
        let report = init_redb_capture_spool_tombstones_non_unix(repo.path(), &mut observer)
            .expect("fresh Redb capture-spool tombstone init must succeed");

        assert_eq!(report.outcome, SpoolTombstoneOutcome::InstalledFresh);
        assert_eq!(
            report.paths.root,
            repo.path().join("private").join("capture-spool")
        );
        assert_eq!(
            report.paths.capacity_lock,
            report.paths.root.join("capacity.lock")
        );

        // AC1: `.libra/private/` and `.libra/private/capture-spool/` have exact `0700` mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for dir in [repo.path().join("private"), report.paths.root.clone()] {
                let meta = fs::symlink_metadata(&dir).expect("stat spool directory");
                assert!(meta.is_dir() && !meta.file_type().is_symlink());
                assert_eq!(
                    meta.permissions().mode() & 0o7777,
                    0o700,
                    "AC1: capture-spool directory must have exact 0700 mode"
                );
            }
        }

        // AC2: `.libra/private/capture-spool/capacity.lock` is a regular file with exact `0600`.
        {
            let lock_meta =
                fs::symlink_metadata(&report.paths.capacity_lock).expect("stat capacity.lock");
            assert!(lock_meta.is_file() && !lock_meta.file_type().is_symlink());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    lock_meta.permissions().mode() & 0o7777,
                    0o600,
                    "AC2: capacity.lock must have exact 0600 mode"
                );
            }
        }

        // AC3: `capture-spool/{tmp,ready,quarantine}` are `0400` regular-file tombstones,
        // and NO temporary staging files remain in `capture-spool/`.
        {
            let mut entries: Vec<String> = fs::read_dir(&report.paths.root)
                .expect("read capture-spool dir")
                .map(|e| {
                    e.expect("dir entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            entries.sort();
            assert_eq!(
                entries,
                vec![
                    "capacity.lock".to_string(),
                    "quarantine".to_string(),
                    "ready".to_string(),
                    "tmp".to_string(),
                ],
                "AC3: capture-spool must contain ONLY capacity.lock and {{tmp,ready,quarantine}} tombstones"
            );

            for tomb in [
                &report.paths.tmp,
                &report.paths.ready,
                &report.paths.quarantine,
            ] {
                let meta = fs::symlink_metadata(tomb).expect("stat tombstone");
                assert!(
                    meta.is_file() && !meta.is_dir() && !meta.file_type().is_symlink(),
                    "AC3: tombstone must be a real regular file (not dir or symlink)"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    assert_eq!(
                        meta.permissions().mode() & 0o7777,
                        0o400,
                        "AC3: tombstone must have exact 0400 mode"
                    );
                }
                assert_eq!(
                    fs::read(tomb).expect("read tombstone sentinel"),
                    CAPTURE_SPOOL_TOMBSTONE_SENTINEL
                );
            }
        }

        // AC4: Exact `publish(tmp, ready, quarantine) -> fsync_dir(private) -> fsync_dir(capture-spool)`
        // durability trace, PLUS injected parent & child `fsync_dir` failure and retry coverage!
        assert_eq!(
            observer.events,
            vec![
                QueueInitEvent::PublishInitialMeta {
                    lane: "capture-spool",
                    file_name: "tmp",
                },
                QueueInitEvent::PublishInitialMeta {
                    lane: "capture-spool",
                    file_name: "ready",
                },
                QueueInitEvent::PublishInitialMeta {
                    lane: "capture-spool",
                    file_name: "quarantine",
                },
                QueueInitEvent::FsyncLaneDir { lane: "private" },
                QueueInitEvent::FsyncLaneDir {
                    lane: "capture-spool",
                },
            ],
            "AC4: fsync_dir(private) and fsync_dir(capture-spool) must run immediately after all 3 tombstones are created"
        );
        {
            // (4a) Inject parent (`private/`) fsync failure on fresh atomic publish (call 0):
            let fail_repo = tempdir().expect("create fsync fail repo");
            let mut fail_obs = QueueInitObserver::failing_fsync_at(0);
            #[cfg(unix)]
            let err = init_redb_capture_spool_tombstones_unix(
                fail_repo.path(),
                UnixExpectedSpoolOwners::current_effective(),
                &mut fail_obs,
            )
            .expect_err("AC4: injected private fsync_dir failure must fail closed");
            #[cfg(not(unix))]
            let err = init_redb_capture_spool_tombstones_non_unix(fail_repo.path(), &mut fail_obs)
                .expect_err("AC4: injected private fsync_dir failure must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");

            // Codex R1 P1 #1 + R2 P1 #1: Retrying on `AlreadyFenced` MUST STILL sync BOTH
            // parent (`private/`) AND child (`capture-spool/`) — failing if EITHER sync fails!
            let mut retry_fail_parent = QueueInitObserver::failing_fsync_at(0);
            #[cfg(unix)]
            let retry_err_parent = init_redb_capture_spool_tombstones_unix(
                fail_repo.path(),
                UnixExpectedSpoolOwners::current_effective(),
                &mut retry_fail_parent,
            )
            .expect_err("AC4 retry: failing parent fsync_dir on AlreadyFenced must fail closed");
            #[cfg(not(unix))]
            let retry_err_parent = init_redb_capture_spool_tombstones_non_unix(
                fail_repo.path(),
                &mut retry_fail_parent,
            )
            .expect_err("AC4 retry: failing parent fsync_dir on AlreadyFenced must fail closed");
            assert_eq!(retry_err_parent.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                retry_fail_parent.events,
                vec![QueueInitEvent::FsyncLaneDir { lane: "private" }]
            );

            let mut retry_fail_child = QueueInitObserver::failing_fsync_at(1);
            #[cfg(unix)]
            let retry_err_child = init_redb_capture_spool_tombstones_unix(
                fail_repo.path(),
                UnixExpectedSpoolOwners::current_effective(),
                &mut retry_fail_child,
            )
            .expect_err("AC4 retry: failing child fsync_dir on AlreadyFenced must fail closed");
            #[cfg(not(unix))]
            let retry_err_child = init_redb_capture_spool_tombstones_non_unix(
                fail_repo.path(),
                &mut retry_fail_child,
            )
            .expect_err("AC4 retry: failing child fsync_dir on AlreadyFenced must fail closed");
            assert_eq!(retry_err_child.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                retry_fail_child.events,
                vec![
                    QueueInitEvent::FsyncLaneDir { lane: "private" },
                    QueueInitEvent::FsyncLaneDir {
                        lane: "capture-spool",
                    },
                ]
            );

            // Succeeding retry executes BOTH parent (`private`) and child (`capture-spool`) fsync!
            let mut retry_ok_obs = QueueInitObserver::new();
            #[cfg(unix)]
            let retry_rep = init_redb_capture_spool_tombstones_unix(
                fail_repo.path(),
                UnixExpectedSpoolOwners::current_effective(),
                &mut retry_ok_obs,
            )
            .expect("AC4 retry: succeeding parent + child fsync_dir must succeed");
            #[cfg(not(unix))]
            let retry_rep =
                init_redb_capture_spool_tombstones_non_unix(fail_repo.path(), &mut retry_ok_obs)
                    .expect("AC4 retry: succeeding parent + child fsync_dir must succeed");
            assert_eq!(retry_rep.outcome, SpoolTombstoneOutcome::AlreadyFenced);
            assert_eq!(
                retry_ok_obs.events,
                vec![
                    QueueInitEvent::FsyncLaneDir { lane: "private" },
                    QueueInitEvent::FsyncLaneDir {
                        lane: "capture-spool",
                    },
                ],
                "AC4 retry: AlreadyFenced path must fsync BOTH parent private/ and capture-spool/"
            );
        }

        // Codex R1 P1 #2: Deterministic mid-publish interleaving with an uncoordinated
        // legacy `ensure_spool` that creates `ready/` directory AFTER our precheck and
        // AFTER `"tmp"` tombstone was published — must fail closed AND roll back `"tmp"`
        // cleanly so ZERO partial tombstones remain alongside the live `ready/` dir!
        {
            let ilv_repo = tempdir().expect("create legacy interleaving repo");
            let spool_root = ilv_repo.path().join("private").join("capture-spool");
            fs::create_dir_all(&spool_root).expect("pre-create empty capture-spool");
            let hook_spool_root = spool_root.clone();
            let mut hook_obs = QueueInitObserver::with_after_publish_hook(move |_lane, slot| {
                if slot == "tmp" {
                    // Simulate racing legacy `ensure_spool` creating live `ready/` dir!
                    fs::create_dir(hook_spool_root.join("ready"))
                        .expect("simulate legacy ensure_spool creating ready/");
                    fs::write(hook_spool_root.join("ready").join("0-live.json"), b"event")
                        .expect("simulate legacy event");
                }
            });
            #[cfg(unix)]
            let err = init_redb_capture_spool_tombstones_unix(
                ilv_repo.path(),
                UnixExpectedSpoolOwners::current_effective(),
                &mut hook_obs,
            )
            .expect_err("mid-publish legacy ready/ creation must fail closed");
            #[cfg(not(unix))]
            let err = init_redb_capture_spool_tombstones_non_unix(ilv_repo.path(), &mut hook_obs)
                .expect_err("mid-publish legacy ready/ creation must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert!(
                !spool_root.join("tmp").exists(),
                "mid-publish rollback must remove newly published tmp tombstone"
            );
            assert!(
                !spool_root.join("quarantine").exists(),
                "quarantine tombstone must not be installed after failure"
            );
            assert!(
                spool_root.join("ready").is_dir(),
                "live legacy ready/ directory must remain completely intact"
            );
            assert_eq!(
                fs::read(spool_root.join("ready").join("0-live.json")).expect("live event intact"),
                b"event"
            );
        }

        // ------------------------------------------------------------------
        // AC5: Legacy SQLite Hook `spool.rs:330` `ensure_spool` immediately fails closed!
        // ------------------------------------------------------------------
        {
            let legacy_err = crate::internal::ai::capture::spool::publish(
                repo.path(),
                false,
                "legacy-sqlite-hook-key",
                b"{\"event\":\"SessionEnd\"}",
                None,
            )
            .expect_err(
                "AC5: legacy spool.rs ensure_spool must fail closed against 0400 tombstones",
            );
            assert!(
                legacy_err
                    .to_string()
                    .contains("capture queue directory is not a private directory"),
                "AC5: unexpected legacy ensure_spool error: {legacy_err:#}"
            );

            // Verify all 3 tombstones and `capacity.lock` remain 100% intact after blocked legacy call.
            for tomb in [
                &report.paths.tmp,
                &report.paths.ready,
                &report.paths.quarantine,
            ] {
                let meta = fs::symlink_metadata(tomb).expect("tombstone still present");
                assert!(meta.is_file() && !meta.is_dir());
                assert_eq!(
                    fs::read(tomb).expect("tombstone bytes preserved"),
                    CAPTURE_SPOOL_TOMBSTONE_SENTINEL
                );
            }
        }

        // Idempotent re-initialization returns `AlreadyFenced` without breaking tombstones,
        // and 8 concurrent threads racing on a fresh repo serialize cleanly behind `capacity.lock`.
        {
            let second =
                init_redb_capture_spool_tombstones(repo.path()).expect("idempotent re-init");
            assert_eq!(second.outcome, SpoolTombstoneOutcome::AlreadyFenced);

            use std::sync::{Arc, Barrier};
            let race_repo = tempdir().expect("create concurrent spool tombstone race repo");
            let race_path = Arc::new(race_repo.path().to_path_buf());
            let barrier = Arc::new(Barrier::new(8));
            let mut handles = Vec::with_capacity(8);
            for _ in 0..8 {
                let p = Arc::clone(&race_path);
                let b = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    b.wait();
                    let mut thread_obs = QueueInitObserver::new();
                    #[cfg(unix)]
                    let rep = init_redb_capture_spool_tombstones_unix(
                        &p,
                        UnixExpectedSpoolOwners::current_effective(),
                        &mut thread_obs,
                    )
                    .expect("concurrent tombstone init must succeed");
                    #[cfg(not(unix))]
                    let rep = init_redb_capture_spool_tombstones_non_unix(&p, &mut thread_obs)
                        .expect("concurrent tombstone init must succeed");
                    // Every returning thread (both InstalledFresh and concurrent AlreadyFenced)
                    // must have durably synced BOTH parent `private/` and `capture-spool/`.
                    assert!(
                        thread_obs
                            .events
                            .contains(&QueueInitEvent::FsyncLaneDir { lane: "private" }),
                        "concurrent returning initializer must fsync parent private/"
                    );
                    assert!(
                        thread_obs.events.contains(&QueueInitEvent::FsyncLaneDir {
                            lane: "capture-spool",
                        }),
                        "concurrent returning initializer must fsync capture-spool/"
                    );
                    for tomb in [&rep.paths.tmp, &rep.paths.ready, &rep.paths.quarantine] {
                        assert_eq!(
                            fs::read(tomb).expect("read tombstone after concurrent init"),
                            CAPTURE_SPOOL_TOMBSTONE_SENTINEL
                        );
                    }
                    rep.outcome
                }));
            }
            let outcomes: Vec<SpoolTombstoneOutcome> = handles
                .into_iter()
                .map(|h| h.join().expect("race worker thread must not panic"))
                .collect();
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|&&o| o == SpoolTombstoneOutcome::InstalledFresh)
                    .count(),
                1,
                "exactly 1 racing initializer must install fresh tombstones: {outcomes:?}"
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|&&o| o == SpoolTombstoneOutcome::AlreadyFenced)
                    .count(),
                7,
                "all other racing initializers must observe AlreadyFenced: {outcomes:?}"
            );
            assert!(
                crate::internal::ai::capture::spool::publish(
                    race_repo.path(),
                    true,
                    "concurrent-legacy-check",
                    b"{}",
                    None,
                )
                .is_err(),
                "legacy spool::publish must fail closed after concurrent tombstone init"
            );
        }

        // Pre-existing live legacy spool directory (`ready/`) fails closed with ZERO partial tombstones!
        {
            let live_repo = tempdir().expect("create live legacy spool repo");
            let spool_root = live_repo.path().join("private").join("capture-spool");
            fs::create_dir_all(spool_root.join("ready")).expect("mkdir live ready/");
            fs::write(spool_root.join("ready").join("0-event.json"), b"live-event")
                .expect("write live event");
            let err = init_redb_capture_spool_tombstones(live_repo.path())
                .expect_err("must fail closed when live capture-spool/ready/ directory exists");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert!(
                !spool_root.join("tmp").exists() && !spool_root.join("quarantine").exists(),
                "pre-check must prevent partial tombstone installation when any slot is a live dir"
            );
            assert_eq!(
                fs::read(spool_root.join("ready").join("0-event.json"))
                    .expect("live event untouched"),
                b"live-event"
            );
        }

        // Symlink and foreign-owner security rejections (Unix).
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt as _, symlink};

            let decoy = tempdir().expect("decoy dir");
            fs::set_permissions(decoy.path(), fs::Permissions::from_mode(0o700))
                .expect("chmod decoy");

            // Symlink at `capture-spool`, `capacity.lock`, and each tombstone slot fails closed.
            {
                let sym_repo = tempdir().expect("symlink capture-spool repo");
                fs::create_dir(sym_repo.path().join("private")).expect("mkdir private");
                symlink(
                    decoy.path(),
                    sym_repo.path().join("private").join("capture-spool"),
                )
                .expect("symlink capture-spool");
                let err = init_redb_capture_spool_tombstones(sym_repo.path())
                    .expect_err("symlinked capture-spool must fail closed");
                assert!(is_capture_queue_symlink_rejected_error(&err), "{err:?}");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
            }
            for target_name in ["capacity.lock", "tmp", "ready", "quarantine"] {
                let sym_repo = tempdir().expect("symlink slot repo");
                let spool_root = sym_repo.path().join("private").join("capture-spool");
                fs::create_dir_all(&spool_root).expect("mkdir spool_root");
                symlink(decoy.path().join("evil"), spool_root.join(target_name))
                    .expect("symlink slot");
                let err = init_redb_capture_spool_tombstones(sym_repo.path())
                    .expect_err(&format!("symlinked {target_name} must fail closed"));
                assert!(
                    is_capture_queue_symlink_rejected_error(&err),
                    "{target_name}: {err:?}"
                );
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert!(!decoy.path().join("evil").exists());
            }

            // Foreign owner UID on `capture-spool` fails closed without mutating permissions.
            let real_owners = UnixExpectedSpoolOwners::current_effective();
            let foreign_uid = real_owners.spool_uid.wrapping_add(1);
            let owner_repo = tempdir().expect("owner mismatch spool repo");
            let spool_root = owner_repo.path().join("private").join("capture-spool");
            fs::create_dir_all(&spool_root).expect("mkdir spool_root");
            fs::set_permissions(&spool_root, fs::Permissions::from_mode(0o755))
                .expect("set 0755 on foreign-owned spool dir");
            let mut obs = QueueInitObserver::new();
            let err = init_redb_capture_spool_tombstones_unix(
                owner_repo.path(),
                UnixExpectedSpoolOwners {
                    spool_uid: foreign_uid,
                    ..real_owners
                },
                &mut obs,
            )
            .expect_err("foreign-owned capture-spool must fail closed");
            assert!(is_capture_queue_owner_mismatch_error(&err), "{err:?}");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::symlink_metadata(&spool_root)
                    .expect("stat spool_root")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o755,
                "foreign-owned capture-spool permissions must never be mutated"
            );
            assert!(!spool_root.join("capacity.lock").exists());
        }
    }
}
