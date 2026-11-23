//! Authoritative DB highwater loading, external mirror persistence, and
//! monotonic segment/offset advance rules for the dual-lane capture queue
//! (`ADR-RB-05` #4, `RB-09`).
//!
//! `capture.redb`'s `QUEUE_HIGHWATER_PRIORITY` and `QUEUE_HIGHWATER_NORMAL`
//! tables are the sole authoritative source of truth. The external
//! `<queue_root>/highwater` file (`HWM1` binary record, 34 bytes with trailing
//! Castagnoli CRC32C) is strictly a non-authoritative mirror refreshed after
//! each committed `CaptureStore::append_capture_batch` transaction.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::storage::{
    capture_store::{CaptureQueueHighwater, CaptureQueueLane, CaptureStore},
    error::{StorageBackendError, StorageResult},
    queue::{LaneTailMeta, crc32c_checksum},
};

/// External advisory highwater mirror filename under `.libra/private/capture-queue/`.
pub const QUEUE_HIGHWATER_MIRROR_FILE: &str = "highwater";

/// 4-byte wire magic identifying the external dual-lane highwater mirror (`HWM1`).
pub const HIGHWATER_MIRROR_MAGIC: [u8; 4] = *b"HWM1";

/// Protocol version stored in `HWM1` bytes `4..6`.
pub const HIGHWATER_MIRROR_VERSION: u16 = 1;

/// Exact byte length of an encoded `HWM1` dual-lane highwater mirror record:
/// `MAGIC(4) + VERSION(2) + PRIORITY_SEG(4) + PRIORITY_OFF(8)
/// + NORMAL_SEG(4) + NORMAL_OFF(8) + CRC32C(4) = 34`.
pub const HIGHWATER_MIRROR_LEN: usize = 34;

const _: () = assert!(
    HIGHWATER_MIRROR_LEN == 4 + 2 + 4 + 8 + 4 + 8 + 4,
    "HWM1 highwater mirror record must be exactly 34 bytes"
);

static HIGHWATER_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_highwater_tmp_name() -> String {
    let seq = HIGHWATER_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{QUEUE_HIGHWATER_MIRROR_FILE}.{}.{seq}.tmp",
        std::process::id()
    )
}

/// Path to the external advisory dual-lane highwater mirror under `queue_root`.
#[must_use]
pub fn highwater_mirror_path(queue_root: &Path) -> PathBuf {
    queue_root.join(QUEUE_HIGHWATER_MIRROR_FILE)
}

/// Dual-lane highwater snapshot pairing `priority/` and `normal/` cursors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueHighwaterSnapshot {
    pub priority: CaptureQueueHighwater,
    pub normal: CaptureQueueHighwater,
}

impl QueueHighwaterSnapshot {
    /// Return the cursor for `lane`.
    #[must_use]
    pub const fn get(&self, lane: CaptureQueueLane) -> CaptureQueueHighwater {
        match lane {
            CaptureQueueLane::Priority => self.priority,
            CaptureQueueLane::Normal => self.normal,
        }
    }

    /// Update the cursor for `lane`.
    pub fn set(&mut self, lane: CaptureQueueLane, highwater: CaptureQueueHighwater) {
        match lane {
            CaptureQueueLane::Priority => self.priority = highwater,
            CaptureQueueLane::Normal => self.normal = highwater,
        }
    }

    /// Encode this snapshot into its fixed 34-byte Castagnoli-CRC32C `HWM1`
    /// mirror representation.
    #[must_use]
    pub fn encode(&self) -> [u8; HIGHWATER_MIRROR_LEN] {
        let mut buf = [0u8; HIGHWATER_MIRROR_LEN];
        buf[0..4].copy_from_slice(&HIGHWATER_MIRROR_MAGIC);
        buf[4..6].copy_from_slice(&HIGHWATER_MIRROR_VERSION.to_le_bytes());
        buf[6..10].copy_from_slice(&self.priority.segment_id.to_le_bytes());
        buf[10..18].copy_from_slice(&self.priority.byte_offset.to_le_bytes());
        buf[18..22].copy_from_slice(&self.normal.segment_id.to_le_bytes());
        buf[22..30].copy_from_slice(&self.normal.byte_offset.to_le_bytes());
        let crc = crc32c_checksum(&buf[0..30]);
        buf[30..34].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Decode and authenticate a 34-byte `HWM1` mirror record.
    pub fn decode(bytes: &[u8]) -> StorageResult<Self> {
        if bytes.len() != HIGHWATER_MIRROR_LEN {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror length is invalid".to_string(),
            });
        }
        if bytes[0..4] != HIGHWATER_MIRROR_MAGIC {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror magic mismatch".to_string(),
            });
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != HIGHWATER_MIRROR_VERSION {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror version unsupported".to_string(),
            });
        }
        let expected_crc = crc32c_checksum(&bytes[0..30]);
        let stored_crc = u32::from_le_bytes([bytes[30], bytes[31], bytes[32], bytes[33]]);
        if expected_crc != stored_crc {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror CRC32C mismatch".to_string(),
            });
        }
        let priority = CaptureQueueHighwater {
            segment_id: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            byte_offset: u64::from_le_bytes([
                bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16],
                bytes[17],
            ]),
        };
        let normal = CaptureQueueHighwater {
            segment_id: u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]),
            byte_offset: u64::from_le_bytes([
                bytes[22], bytes[23], bytes[24], bytes[25], bytes[26], bytes[27], bytes[28],
                bytes[29],
            ]),
        };
        Ok(Self { priority, normal })
    }
}

/// Compute the authoritative next highwater cursor after consuming frames up
/// to `batch_end_offset` in segment `current_hw.segment_id` (`ADR-RB-05` #4 ②).
///
/// Enforces all three highwater advance invariants:
/// - **Active segment (`S == active_seg`, rule ② (a)):** advances strictly to
///   `(S, batch_end_offset)` (at most `(S, committed_len)`) and NEVER jumps to
///   `(S + 1, 0)`.
/// - **Sealed segment (`S < active_seg`, rule ② (b)/(c)):** intermediate
///   batches (`batch_end_offset < sealed_len(S)`) advance strictly to
///   `(S, batch_end_offset)`; ONLY when `batch_end_offset == sealed_len(S)`
///   does the cursor cross atomically to `(S + 1, 0)`.
pub fn compute_next_highwater(
    current_hw: CaptureQueueHighwater,
    meta: &LaneTailMeta,
    batch_end_offset: u64,
) -> StorageResult<CaptureQueueHighwater> {
    if current_hw.segment_id > meta.active_seg {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 active_seg regressed behind DB highwater".to_string(),
        });
    }
    if current_hw.segment_id < meta.min_live_seg {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue LTM2 min_live_seg surpassed unconsumed DB highwater".to_string(),
        });
    }
    if batch_end_offset < current_hw.byte_offset {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue batch end offset regressed behind current highwater".to_string(),
        });
    }

    if current_hw.segment_id == meta.active_seg {
        if batch_end_offset > meta.committed_len {
            return Err(StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue batch end offset exceeds active segment committed_len"
                    .to_string(),
            });
        }
        return Ok(CaptureQueueHighwater {
            segment_id: current_hw.segment_id,
            byte_offset: batch_end_offset,
        });
    }

    let sealed_entry = meta
        .sealed
        .iter()
        .find(|entry| entry.seg_id == current_hw.segment_id)
        .ok_or_else(|| StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue sealed segment missing from LTM2 sealed table".to_string(),
        })?;
    if batch_end_offset > sealed_entry.sealed_len {
        return Err(StorageBackendError::CorruptCommittedFrame {
            detail: "capture queue batch end offset exceeds LTM2 sealed_len".to_string(),
        });
    }
    if batch_end_offset == sealed_entry.sealed_len {
        let next_seg = current_hw.segment_id.checked_add(1).ok_or_else(|| {
            StorageBackendError::CorruptCommittedFrame {
                detail: "capture queue segment id overflow on highwater advance".to_string(),
            }
        })?;
        Ok(CaptureQueueHighwater {
            segment_id: next_seg,
            byte_offset: 0,
        })
    } else {
        Ok(CaptureQueueHighwater {
            segment_id: current_hw.segment_id,
            byte_offset: batch_end_offset,
        })
    }
}

/// Load the authoritative dual-lane highwater snapshot from `CaptureStore`
/// (`QUEUE_HIGHWATER_PRIORITY` and `QUEUE_HIGHWATER_NORMAL`).
pub async fn load_authoritative_highwater(
    store: &dyn CaptureStore,
) -> StorageResult<QueueHighwaterSnapshot> {
    let priority = store
        .get_queue_highwater(CaptureQueueLane::Priority)
        .await?;
    let normal = store.get_queue_highwater(CaptureQueueLane::Normal).await?;
    Ok(QueueHighwaterSnapshot { priority, normal })
}

/// Load the authoritative DB highwater and ensure the external advisory mirror
/// file under `queue_root` is synchronized to match it.
pub async fn sync_highwater_mirror_from_store(
    store: &dyn CaptureStore,
    queue_root: &Path,
) -> StorageResult<QueueHighwaterSnapshot> {
    let authoritative = load_authoritative_highwater(store).await?;
    let current_mirror = read_highwater_mirror(queue_root)?;
    if current_mirror != Some(authoritative) {
        write_highwater_mirror(queue_root, &authoritative)?;
    }
    Ok(authoritative)
}

/// Read the external advisory highwater mirror under `queue_root`.
///
/// Returns `Ok(None)` when the mirror file is absent or fails `HWM1` format/CRC
/// validation (the DB remains authoritative and callers rebuild the mirror).
/// Symlinks, directories, or foreign-owned files fail closed immediately.
pub fn read_highwater_mirror(queue_root: &Path) -> StorageResult<Option<QueueHighwaterSnapshot>> {
    #[cfg(unix)]
    {
        unix_hw::read_highwater_mirror_unix(queue_root)
    }
    #[cfg(not(unix))]
    {
        non_unix_hw::read_highwater_mirror_non_unix(queue_root)
    }
}

/// Atomically write the external advisory highwater mirror under `queue_root`
/// (`tmp -> sync_data -> rename -> fsync_dir(queue_root)`, `0600`, `O_NOFOLLOW`).
pub fn write_highwater_mirror(
    queue_root: &Path,
    snapshot: &QueueHighwaterSnapshot,
) -> StorageResult<()> {
    let encoded = snapshot.encode();
    #[cfg(unix)]
    {
        unix_hw::write_highwater_mirror_unix(queue_root, &encoded)
    }
    #[cfg(not(unix))]
    {
        non_unix_hw::write_highwater_mirror_non_unix(queue_root, &encoded)
    }
}

#[cfg(unix)]
mod unix_hw {
    use std::{
        ffi::{CStr, CString, OsStr},
        io::{Read as _, Write as _},
        os::unix::{
            ffi::OsStrExt as _,
            io::{AsRawFd as _, FromRawFd as _, OwnedFd},
        },
    };

    use super::*;
    use crate::storage::{
        manifest::PRIVATE_SUBDIR,
        queue::{
            QUEUE_DIR_MODE, QUEUE_FILE_MODE, map_queue_owner_mismatch_err, map_queue_symlink_err,
        },
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
        // SAFETY: `fd` is a valid open descriptor and `st` is valid writable memory.
        let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
        if rc != 0 {
            return Err(StorageBackendError::io(context, io::Error::last_os_error()));
        }
        // SAFETY: `fstat` succeeded and fully initialized `st`.
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
        let cname = osstr_to_cstring(name, "open capture queue directory")?;
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

    pub(super) fn read_highwater_mirror_unix(
        queue_root: &Path,
    ) -> StorageResult<Option<QueueHighwaterSnapshot>> {
        let expected_uid = current_euid();
        let root_fd = open_verified_queue_root(queue_root, expected_uid)?;
        let cname = osstr_to_cstring(
            OsStr::new(QUEUE_HIGHWATER_MIRROR_FILE),
            "open capture queue highwater mirror",
        )?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: `root_fd` is valid and `cname` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(root_fd.as_raw_fd(), cname.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            if is_symlink_errno_at(&err, &root_fd, &cname) {
                return Err(map_queue_symlink_err("capture queue highwater mirror"));
            }
            return Err(StorageBackendError::io(
                "open capture queue highwater mirror",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let st = fstat_fd(&fd, "stat capture queue highwater mirror")?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror is not a regular file".to_string(),
            });
        }
        if st.st_uid != expected_uid {
            return Err(map_queue_owner_mismatch_err(
                "capture queue highwater mirror",
            ));
        }
        let file = File::from(fd);
        let mut buf = Vec::with_capacity(HIGHWATER_MIRROR_LEN + 1);
        file.take((HIGHWATER_MIRROR_LEN + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| StorageBackendError::io("read capture queue highwater mirror", e))?;
        match QueueHighwaterSnapshot::decode(&buf) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(_) => Ok(None),
        }
    }

    pub(super) fn write_highwater_mirror_unix(
        queue_root: &Path,
        encoded: &[u8; HIGHWATER_MIRROR_LEN],
    ) -> StorageResult<()> {
        let expected_uid = current_euid();
        let root_fd = open_verified_queue_root(queue_root, expected_uid)?;

        // If an existing mirror entry is a symlink, directory, or foreign-owned,
        // fail closed before renaming over it.
        let ctarget = osstr_to_cstring(
            OsStr::new(QUEUE_HIGHWATER_MIRROR_FILE),
            "check capture queue highwater mirror",
        )?;
        let mut target_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `root_fd` is valid and `ctarget` is NUL-terminated.
        let stat_rc = unsafe {
            libc::fstatat(
                root_fd.as_raw_fd(),
                ctarget.as_ptr(),
                target_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if stat_rc == 0 {
            // SAFETY: `fstatat` succeeded.
            let target_st = unsafe { target_st.assume_init() };
            let ftype = target_st.st_mode & libc::S_IFMT;
            if ftype == libc::S_IFLNK {
                return Err(map_queue_symlink_err("capture queue highwater mirror"));
            }
            if ftype != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture queue highwater mirror is not a regular file".to_string(),
                });
            }
            if target_st.st_uid != expected_uid {
                return Err(map_queue_owner_mismatch_err(
                    "capture queue highwater mirror",
                ));
            }
        } else {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(StorageBackendError::io(
                    "stat existing capture queue highwater mirror",
                    err,
                ));
            }
        }

        let tmp_name = next_highwater_tmp_name();
        let ctmp = osstr_to_cstring(OsStr::new(&tmp_name), "create highwater mirror temp")?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `root_fd` is valid and `ctmp` is NUL-terminated.
        let raw_fd = unsafe { libc::openat(root_fd.as_raw_fd(), ctmp.as_ptr(), flags, 0o600) };
        if raw_fd < 0 {
            return Err(StorageBackendError::io(
                "create capture queue highwater mirror temp file",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `raw_fd >= 0` is exclusively owned here.
        let tmp_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let cleanup_tmp = |root: &OwnedFd, name: &CStr| {
            // SAFETY: best-effort cleanup of temp file we just created.
            unsafe {
                let _ = libc::unlinkat(root.as_raw_fd(), name.as_ptr(), 0);
            }
        };

        // SAFETY: `tmp_fd` is valid.
        if unsafe { libc::fchmod(tmp_fd.as_raw_fd(), QUEUE_FILE_MODE as libc::mode_t) } != 0 {
            let err = io::Error::last_os_error();
            drop(tmp_fd);
            cleanup_tmp(&root_fd, &ctmp);
            return Err(StorageBackendError::io(
                "chmod capture queue highwater mirror temp file",
                err,
            ));
        }
        let mut tmp_file = File::from(tmp_fd);
        if let Err(err) = tmp_file
            .write_all(encoded)
            .and_then(|()| tmp_file.sync_data())
        {
            drop(tmp_file);
            cleanup_tmp(&root_fd, &ctmp);
            return Err(StorageBackendError::io(
                "write and sync capture queue highwater mirror temp file",
                err,
            ));
        }
        drop(tmp_file);

        // SAFETY: `root_fd` is valid and `ctmp`/`ctarget` are NUL-terminated.
        let ren_rc = unsafe {
            libc::renameat(
                root_fd.as_raw_fd(),
                ctmp.as_ptr(),
                root_fd.as_raw_fd(),
                ctarget.as_ptr(),
            )
        };
        if ren_rc != 0 {
            let err = io::Error::last_os_error();
            cleanup_tmp(&root_fd, &ctmp);
            return Err(StorageBackendError::io(
                "atomically publish capture queue highwater mirror",
                err,
            ));
        }

        fsync_fd(
            &root_fd,
            "fsync capture queue root after publishing highwater mirror",
        )?;
        Ok(())
    }
}

#[cfg(not(unix))]
mod non_unix_hw {
    use std::{
        fs,
        io::{Read as _, Write as _},
    };

    use super::*;
    use crate::storage::queue::map_queue_symlink_err;

    #[cfg(windows)]
    mod win_hw {
        use std::os::windows::{
            ffi::OsStrExt as _,
            io::{AsRawHandle as _, FromRawHandle as _},
        };

        use windows_sys::Win32::{
            Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                FlushFileBuffers, GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING,
                MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
            },
        };

        use super::*;

        pub(super) struct WinHandleGuard(HANDLE);

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
            let info = file_info_by_handle(handle, "stat capture queue highwater mirror handle")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} is not a regular file"),
                });
            }
            Ok(())
        }

        fn verify_handle_dir(handle: HANDLE, subject: &'static str) -> StorageResult<()> {
            let info = file_info_by_handle(handle, "stat capture queue directory handle")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return Err(map_queue_symlink_err(subject));
            }
            if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("{subject} is not a directory"),
                });
            }
            Ok(())
        }

        pub(super) fn open_nofollow_regular_file(
            path: &Path,
            access: u32,
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
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    creation,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(StorageBackendError::io(context, io::Error::last_os_error()));
            }
            // SAFETY: `handle` is exclusively owned here and wrapped immediately for RAII cleanup.
            let file = unsafe { File::from_raw_handle(handle as *mut std::ffi::c_void) };
            verify_handle_regular_file(file.as_raw_handle() as HANDLE, subject)?;
            Ok(file)
        }

        /// Open and PIN a Windows directory handle with
        /// `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT` and
        /// `FILE_SHARE_READ | FILE_SHARE_WRITE` (explicitly omitting
        /// `FILE_SHARE_DELETE`), verifying directly on the opened handle that it
        /// is a real non-reparse directory. Holding the returned
        /// [`WinHandleGuard`] prevents directory rename/delete/junction-swap
        /// across the entire operation.
        pub(super) fn pin_dir_not_reparse(
            dir: &Path,
            subject: &'static str,
        ) -> StorageResult<WinHandleGuard> {
            let wide = to_wide_nul(dir);
            // SAFETY: `wide` is NUL-terminated. Omitting `FILE_SHARE_DELETE`
            // pins the directory against rename/delete/junction replacement
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
                    "open capture queue directory for verification",
                    io::Error::last_os_error(),
                ));
            }
            let guard = WinHandleGuard(handle);
            verify_handle_dir(guard.0, subject)?;
            Ok(guard)
        }

        pub(super) fn pin_queue_root_chain(
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

        pub(super) fn atomic_replace(src: &Path, dst: &Path) -> StorageResult<()> {
            let src_w = to_wide_nul(src);
            let dst_w = to_wide_nul(dst);
            // SAFETY: both slices are NUL-terminated.
            let rc = unsafe {
                MoveFileExW(
                    src_w.as_ptr(),
                    dst_w.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if rc == 0 {
                return Err(StorageBackendError::io(
                    "atomically publish capture queue highwater mirror",
                    io::Error::last_os_error(),
                ));
            }
            Ok(())
        }

        pub(super) fn flush_dir(dir: &Path) -> StorageResult<()> {
            let wide = to_wide_nul(dir);
            // SAFETY: `wide` is NUL-terminated. Omitting `FILE_SHARE_DELETE`
            // keeps the directory pinned alongside any existing pin handle.
            let handle: HANDLE = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(StorageBackendError::io(
                    "open capture queue root for flush",
                    io::Error::last_os_error(),
                ));
            }
            let guard = WinHandleGuard(handle);
            verify_handle_dir(guard.0, "capture queue root directory")?;
            // SAFETY: `guard.0` is a valid open directory handle.
            let flush_rc = unsafe { FlushFileBuffers(guard.0) };
            if flush_rc == 0 {
                return Err(StorageBackendError::io(
                    "flush capture queue root after publishing highwater mirror",
                    io::Error::last_os_error(),
                ));
            }
            Ok(())
        }
    }

    pub(super) fn read_highwater_mirror_non_unix(
        queue_root: &Path,
    ) -> StorageResult<Option<QueueHighwaterSnapshot>> {
        #[cfg(windows)]
        {
            // Keep `private/` and `queue_root` pinned (without `FILE_SHARE_DELETE`)
            // throughout child open and read so neither directory can be swapped
            // with a junction between verification and child access.
            let _dir_pins = win_hw::pin_queue_root_chain(queue_root)?;
            let target = highwater_mirror_path(queue_root);
            let file = match win_hw::open_nofollow_regular_file(
                &target,
                windows_sys::Win32::Foundation::GENERIC_READ,
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                "capture queue highwater mirror",
                "open capture queue highwater mirror",
            ) {
                Ok(f) => f,
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => return Ok(None),
                Err(err) => return Err(err),
            };
            let mut buf = Vec::with_capacity(HIGHWATER_MIRROR_LEN + 1);
            file.take((HIGHWATER_MIRROR_LEN + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| StorageBackendError::io("read capture queue highwater mirror", e))?;
            match QueueHighwaterSnapshot::decode(&buf) {
                Ok(snapshot) => Ok(Some(snapshot)),
                Err(_) => Ok(None),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = queue_root;
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror is unsupported on this platform"
                    .to_string(),
            })
        }
    }

    pub(super) fn write_highwater_mirror_non_unix(
        queue_root: &Path,
        encoded: &[u8; HIGHWATER_MIRROR_LEN],
    ) -> StorageResult<()> {
        #[cfg(windows)]
        {
            // Keep `private/` and `queue_root` pinned (without `FILE_SHARE_DELETE`)
            // across target validation, temp creation, atomic replace, and dir flush.
            let _dir_pins = win_hw::pin_queue_root_chain(queue_root)?;
            let target = highwater_mirror_path(queue_root);
            // If an existing mirror entry is present, validate its handle directly
            // (reparse point / directory fail closed) and release before replace.
            match win_hw::open_nofollow_regular_file(
                &target,
                windows_sys::Win32::Foundation::GENERIC_READ,
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                "capture queue highwater mirror",
                "stat capture queue highwater mirror",
            ) {
                Ok(existing) => drop(existing),
                Err(StorageBackendError::Io {
                    kind: io::ErrorKind::NotFound,
                    ..
                }) => {}
                Err(err) => return Err(err),
            }
            let tmp_path = queue_root.join(next_highwater_tmp_name());
            let mut tmp_file = win_hw::open_nofollow_regular_file(
                &tmp_path,
                windows_sys::Win32::Foundation::GENERIC_WRITE,
                windows_sys::Win32::Storage::FileSystem::CREATE_NEW,
                "capture queue highwater mirror temp",
                "create capture queue highwater mirror temp",
            )?;
            if let Err(err) = tmp_file
                .write_all(encoded)
                .and_then(|()| tmp_file.sync_data())
            {
                drop(tmp_file);
                let _ = fs::remove_file(&tmp_path);
                return Err(StorageBackendError::io(
                    "write capture queue highwater mirror temp",
                    err,
                ));
            }
            drop(tmp_file);
            if let Err(err) = win_hw::atomic_replace(&tmp_path, &target) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
            win_hw::flush_dir(queue_root)?;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (queue_root, encoded);
            Err(StorageBackendError::BackendInternal {
                detail: "capture queue highwater mirror is unsupported on this platform"
                    .to_string(),
            })
        }
    }
}
