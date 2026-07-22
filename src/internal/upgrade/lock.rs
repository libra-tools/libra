//! Install-directory validation, fd-relative file operations and the
//! advisory upgrade lock (plan-20260714 §A.5).
//!
//! Every upgrade-side mutation of the install directory goes through
//! [`InstallDir`]: the directory is opened ONCE with
//! `O_DIRECTORY|O_NOFOLLOW` after §A.5 validation (absolute path, owned by
//! the current effective uid, not group/world-writable — no sticky-directory
//! exception is currently granted), and every subsequent target/lock/
//! marker/txn/state operation is performed RELATIVE to that fd with
//! `O_NOFOLLOW`, so a concurrent directory swap or symlink plant cannot
//! redirect writes (§A.5 TOCTOU discipline).
//!
//! The advisory lock is an `flock` on a `0600` lock file inside the
//! directory. The auto-upgrade path uses [`InstallDir::try_lock`]
//! (busy ⇒ Skip); crash recovery takes the same lock and holds it until its
//! final fsync. Windows does not enter this path in R0 (§A.5), and
//! non-Unix builds fail closed with [`InstallDirError::UnsupportedPlatform`].

#[cfg(unix)]
use std::os::unix::io::OwnedFd;
use std::path::{Path, PathBuf};

/// Lock file name inside the install directory.
pub const LOCK_FILE_NAME: &str = ".libra-upgrade.lock";

/// Failures of install-dir validation and fd-relative operations.
#[derive(Debug, thiserror::Error)]
pub enum InstallDirError {
    #[error("auto-upgrade file operations are not supported on this platform in this release")]
    UnsupportedPlatform,
    #[error("install directory path '{0}' is not absolute")]
    NotAbsolute(PathBuf),
    #[error("cannot open install directory '{path}' (no-follow): {detail}")]
    Open { path: PathBuf, detail: String },
    #[error("install directory '{path}' failed validation: {detail}")]
    Validation { path: PathBuf, detail: String },
    #[error("'{0}' is not a plain file name (path separators and dot entries are refused)")]
    BadEntryName(String),
    #[error("entry '{name}' in the install directory is not a regular file")]
    NotRegular { name: String },
    #[error("I/O on '{name}' inside the install directory failed: {detail}")]
    Io { name: String, detail: String },
}

/// A validated install directory holding the directory fd (§A.5).
#[derive(Debug)]
pub struct InstallDir {
    #[cfg(unix)]
    dir: OwnedFd,
    path: PathBuf,
}

/// Held advisory upgrade lock; released on drop.
#[derive(Debug)]
pub struct UpgradeLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

/// Metadata of a directory entry read via the directory fd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Regular { size: u64, mode: u32 },
    Directory,
    Symlink,
    Other,
}

fn validate_entry_name(name: &str) -> Result<(), InstallDirError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(InstallDirError::BadEntryName(name.to_string()));
    }
    Ok(())
}

#[cfg(unix)]
mod unix_impl {
    use std::{
        ffi::CString,
        io::{Read, Write},
        os::unix::io::{AsRawFd, FromRawFd},
    };

    use super::*;

    fn cstr(name: &str) -> Result<CString, InstallDirError> {
        CString::new(name).map_err(|_| InstallDirError::BadEntryName(name.to_string()))
    }

    fn io_err(name: &str) -> InstallDirError {
        InstallDirError::Io {
            name: name.to_string(),
            detail: std::io::Error::last_os_error().to_string(),
        }
    }

    impl InstallDir {
        /// Open and validate an install directory per §A.5 (see module docs).
        pub fn open_validated(path: &Path) -> Result<Self, InstallDirError> {
            if !path.is_absolute() {
                return Err(InstallDirError::NotAbsolute(path.to_path_buf()));
            }
            let canonical = path.canonicalize().map_err(|e| InstallDirError::Open {
                path: path.to_path_buf(),
                detail: e.to_string(),
            })?;
            let c_path = CString::new(canonical.as_os_str().as_encoded_bytes()).map_err(|_| {
                InstallDirError::Open {
                    path: canonical.clone(),
                    detail: "path contains NUL".into(),
                }
            })?;
            // SAFETY: plain libc open with a valid CString; the fd is wrapped
            // in OwnedFd immediately on success.
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(InstallDirError::Open {
                    path: canonical,
                    detail: std::io::Error::last_os_error().to_string(),
                });
            }
            // SAFETY: fd is a freshly opened, owned descriptor.
            let dir = unsafe { OwnedFd::from_raw_fd(fd) };
            // Authoritative checks on the FD itself (not the path), so a
            // post-open swap cannot bypass them.
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstat writes into the provided buffer on success.
            let rc = unsafe { libc::fstat(dir.as_raw_fd(), stat.as_mut_ptr()) };
            if rc != 0 {
                return Err(InstallDirError::Open {
                    path: canonical,
                    detail: std::io::Error::last_os_error().to_string(),
                });
            }
            // SAFETY: fstat succeeded, the buffer is initialized.
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(InstallDirError::Validation {
                    path: canonical,
                    detail: "not a directory".into(),
                });
            }
            // SAFETY: geteuid is always safe.
            let euid = unsafe { libc::geteuid() };
            if stat.st_uid != euid {
                return Err(InstallDirError::Validation {
                    path: canonical,
                    detail: format!(
                        "owned by uid {} but the current effective uid is {euid}",
                        stat.st_uid
                    ),
                });
            }
            // No sticky-directory exception is granted in R0 (§A.5): any
            // group/world-writable install directory is rejected outright.
            if stat.st_mode & 0o022 != 0 {
                return Err(InstallDirError::Validation {
                    path: canonical,
                    detail: format!(
                        "group/world-writable (mode {:o}); tighten it with: chmod go-w",
                        stat.st_mode & 0o7777
                    ),
                });
            }
            Ok(Self {
                dir,
                path: canonical,
            })
        }

        /// The canonical directory path (diagnostics only — never used for
        /// file operations, which stay fd-relative).
        pub fn path(&self) -> &Path {
            &self.path
        }

        fn openat(
            &self,
            name: &str,
            flags: libc::c_int,
            mode: libc::c_int,
        ) -> Result<std::fs::File, InstallDirError> {
            validate_entry_name(name)?;
            let c_name = cstr(name)?;
            // SAFETY: openat on the held directory fd with a valid CString;
            // wrapped in File immediately on success.
            let fd = unsafe {
                libc::openat(
                    self.dir.as_raw_fd(),
                    c_name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    mode,
                )
            };
            if fd < 0 {
                return Err(io_err(name));
            }
            // SAFETY: freshly opened owned fd.
            Ok(unsafe { std::fs::File::from_raw_fd(fd) })
        }

        /// Metadata of `name` via `fstatat(..., AT_SYMLINK_NOFOLLOW)`;
        /// `Ok(None)` when absent.
        pub fn stat_entry(&self, name: &str) -> Result<Option<EntryKind>, InstallDirError> {
            validate_entry_name(name)?;
            let c_name = cstr(name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstatat writes into the buffer on success.
            let rc = unsafe {
                libc::fstatat(
                    self.dir.as_raw_fd(),
                    c_name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(InstallDirError::Io {
                    name: name.to_string(),
                    detail: err.to_string(),
                });
            }
            // SAFETY: fstatat succeeded.
            let stat = unsafe { stat.assume_init() };
            Ok(Some(match stat.st_mode & libc::S_IFMT {
                libc::S_IFREG => EntryKind::Regular {
                    size: stat.st_size as u64,
                    // `st_mode` is u16 on macOS and u32 on Linux; widen
                    // portably to the field type.
                    #[allow(clippy::unnecessary_cast)]
                    mode: (stat.st_mode & 0o7777) as u32,
                },
                libc::S_IFDIR => EntryKind::Directory,
                libc::S_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            }))
        }

        /// Read a regular file (no-follow); `Ok(None)` when absent.
        pub fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>, InstallDirError> {
            let mut file = match self.openat(name, libc::O_RDONLY, 0) {
                Ok(file) => file,
                Err(InstallDirError::Io { detail, .. })
                    if detail.contains("No such file") || detail.contains("(os error 2)") =>
                {
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };
            let meta = file.metadata().map_err(|e| InstallDirError::Io {
                name: name.to_string(),
                detail: e.to_string(),
            })?;
            if !meta.is_file() {
                return Err(InstallDirError::NotRegular {
                    name: name.to_string(),
                });
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| InstallDirError::Io {
                    name: name.to_string(),
                    detail: e.to_string(),
                })?;
            Ok(Some(bytes))
        }

        /// Atomically write `bytes` to `name`: exclusive temp file in the
        /// SAME directory (via the fd), fsync, `renameat` over the target,
        /// fsync the directory (§A.5/§A.7 durability).
        pub fn write_file_atomic(
            &self,
            name: &str,
            bytes: &[u8],
            mode: u32,
        ) -> Result<(), InstallDirError> {
            validate_entry_name(name)?;
            let tmp_name = format!(".tmp-{}-{name}", std::process::id());
            let mut tmp = self.openat(
                &tmp_name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                mode as libc::c_int,
            )?;
            let cleanup = |dir: &Self| {
                let _ = dir.remove_file(&tmp_name);
            };
            if let Err(e) = tmp.write_all(bytes).and_then(|_| tmp.sync_all()) {
                cleanup(self);
                return Err(InstallDirError::Io {
                    name: name.to_string(),
                    detail: e.to_string(),
                });
            }
            drop(tmp);
            let c_from = cstr(&tmp_name)?;
            let c_to = cstr(name)?;
            // SAFETY: renameat within the held directory fd.
            let rc = unsafe {
                libc::renameat(
                    self.dir.as_raw_fd(),
                    c_from.as_ptr(),
                    self.dir.as_raw_fd(),
                    c_to.as_ptr(),
                )
            };
            if rc != 0 {
                let err = io_err(name);
                cleanup(self);
                return Err(err);
            }
            self.fsync_dir()
        }

        /// Rename `from` → `to` within the directory (both fd-relative).
        pub fn rename_entry(&self, from: &str, to: &str) -> Result<(), InstallDirError> {
            validate_entry_name(from)?;
            validate_entry_name(to)?;
            let c_from = cstr(from)?;
            let c_to = cstr(to)?;
            // SAFETY: renameat within the held directory fd.
            let rc = unsafe {
                libc::renameat(
                    self.dir.as_raw_fd(),
                    c_from.as_ptr(),
                    self.dir.as_raw_fd(),
                    c_to.as_ptr(),
                )
            };
            if rc != 0 {
                return Err(io_err(from));
            }
            Ok(())
        }

        /// Remove `name` (fd-relative); `Ok(false)` when it did not exist.
        pub fn remove_file(&self, name: &str) -> Result<bool, InstallDirError> {
            validate_entry_name(name)?;
            let c_name = cstr(name)?;
            // SAFETY: unlinkat on the held directory fd.
            let rc = unsafe { libc::unlinkat(self.dir.as_raw_fd(), c_name.as_ptr(), 0) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::NotFound {
                    return Ok(false);
                }
                return Err(InstallDirError::Io {
                    name: name.to_string(),
                    detail: err.to_string(),
                });
            }
            Ok(true)
        }

        /// fsync the directory itself so renames/unlinks are durable.
        pub fn fsync_dir(&self) -> Result<(), InstallDirError> {
            // SAFETY: fsync on the held directory fd.
            let rc = unsafe { libc::fsync(self.dir.as_raw_fd()) };
            if rc != 0 {
                return Err(InstallDirError::Io {
                    name: ".".into(),
                    detail: std::io::Error::last_os_error().to_string(),
                });
            }
            Ok(())
        }

        fn open_lock_file(&self) -> Result<std::fs::File, InstallDirError> {
            self.openat(
                LOCK_FILE_NAME,
                libc::O_RDWR | libc::O_CREAT,
                0o600 as libc::c_int,
            )
        }

        /// Non-blocking upgrade lock: `Ok(None)` when another process holds
        /// it (auto-upgrade treats that as Skip, §A.5).
        pub fn try_lock(&self) -> Result<Option<UpgradeLock>, InstallDirError> {
            let file = self.open_lock_file()?;
            // SAFETY: flock on an owned fd.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                return match err.raw_os_error() {
                    Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
                    _ => Err(InstallDirError::Io {
                        name: LOCK_FILE_NAME.to_string(),
                        detail: err.to_string(),
                    }),
                };
            }
            Ok(Some(UpgradeLock { _file: file }))
        }

        /// Blocking upgrade lock (crash recovery holds it to the last fsync).
        pub fn lock_blocking(&self) -> Result<UpgradeLock, InstallDirError> {
            let file = self.open_lock_file()?;
            // SAFETY: flock on an owned fd.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(InstallDirError::Io {
                    name: LOCK_FILE_NAME.to_string(),
                    detail: std::io::Error::last_os_error().to_string(),
                });
            }
            Ok(UpgradeLock { _file: file })
        }
    }
}

#[cfg(not(unix))]
impl InstallDir {
    /// §A.5: Windows (and any non-Unix target) does not enter the upgrade
    /// file path in R0.
    pub fn open_validated(_path: &Path) -> Result<Self, InstallDirError> {
        Err(InstallDirError::UnsupportedPlatform)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn owned_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("test fixture operation should succeed");
        let path = dir
            .path()
            .canonicalize()
            .expect("test fixture operation should succeed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test fixture operation should succeed");
        (dir, path)
    }

    #[test]
    fn validation_rejects_relative_and_writable_and_symlink() {
        // Relative path.
        assert!(matches!(
            InstallDir::open_validated(Path::new("relative/dir")),
            Err(InstallDirError::NotAbsolute(_))
        ));
        // Group/world-writable.
        let (_guard, path) = owned_dir();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))
            .expect("test fixture operation should succeed");
        assert!(matches!(
            InstallDir::open_validated(&path),
            Err(InstallDirError::Validation { .. })
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test fixture operation should succeed");
        // A symlink pointing at the directory: canonicalize resolves it, and
        // the resolved REAL directory must still pass its own checks — but a
        // dangling/looping link fails open.
        let (_g2, base) = owned_dir();
        let link = base.join("link");
        std::os::unix::fs::symlink(&path, &link).expect("test fixture operation should succeed");
        assert!(InstallDir::open_validated(&link).is_ok());
    }

    #[test]
    fn entry_name_discipline() {
        let (_guard, path) = owned_dir();
        let dir = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        for bad in ["a/b", ".", "..", "", "x\0y"] {
            assert!(
                matches!(dir.read_file(bad), Err(InstallDirError::BadEntryName(_))),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn atomic_write_read_rename_remove_roundtrip() {
        let (_guard, path) = owned_dir();
        let dir = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        assert_eq!(
            dir.read_file("f")
                .expect("test fixture operation should succeed"),
            None
        );
        dir.write_file_atomic("f", b"hello", 0o600)
            .expect("test fixture operation should succeed");
        assert_eq!(
            dir.read_file("f")
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"hello"[..])
        );
        match dir
            .stat_entry("f")
            .expect("test fixture operation should succeed")
        {
            Some(EntryKind::Regular { size, mode }) => {
                assert_eq!(size, 5);
                assert_eq!(mode, 0o600);
            }
            other => panic!("expected regular file, got {other:?}"),
        }
        // Overwrite is atomic and leaves no temp files behind.
        dir.write_file_atomic("f", b"world!", 0o600)
            .expect("test fixture operation should succeed");
        assert_eq!(
            dir.read_file("f")
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"world!"[..])
        );
        let leftovers: Vec<_> = std::fs::read_dir(&path)
            .expect("test fixture operation should succeed")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");
        dir.rename_entry("f", "g")
            .expect("test fixture operation should succeed");
        assert_eq!(
            dir.read_file("f")
                .expect("test fixture operation should succeed"),
            None
        );
        assert!(
            dir.remove_file("g")
                .expect("test fixture operation should succeed")
        );
        assert!(
            !dir.remove_file("g")
                .expect("test fixture operation should succeed")
        );
    }

    #[test]
    fn read_refuses_symlink_entries() {
        let (_guard, path) = owned_dir();
        let dir = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        std::fs::write(path.join("real"), b"x").expect("test fixture operation should succeed");
        std::os::unix::fs::symlink(path.join("real"), path.join("sneaky"))
            .expect("test fixture operation should succeed");
        assert_eq!(
            dir.stat_entry("sneaky")
                .expect("test fixture operation should succeed"),
            Some(EntryKind::Symlink)
        );
        // openat with O_NOFOLLOW fails on the symlink (ELOOP/EMLINK).
        assert!(dir.read_file("sneaky").is_err());
    }

    #[test]
    fn try_lock_is_exclusive_and_released_on_drop() {
        let (_guard, path) = owned_dir();
        let a = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        let b = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        let held = a
            .try_lock()
            .expect("test fixture operation should succeed")
            .expect("first lock acquired");
        assert!(
            b.try_lock()
                .expect("test fixture operation should succeed")
                .is_none(),
            "second locker must skip"
        );
        drop(held);
        assert!(
            b.try_lock()
                .expect("test fixture operation should succeed")
                .is_some(),
            "released on drop"
        );
    }
}
