//! Fail-fast exclusion for one pinned physical worktree.
//!
//! The private gitdir is the existing trusted metadata boundary. We pin its
//! no-follow `info` directory and reject a changed parent before returning;
//! this is not an atomic guarantee against arbitrary ancestor replacement.
//! Other writers must not replace the directory or the persistent lock file.

use std::{
    fs::{self, File, Metadata, OpenOptions, TryLockError},
    io,
    path::Path,
};

use super::{OperationError, PinnedRequestScope};
use crate::utils::beneath::entry_identity_beneath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaseFilePermissions {
    Umask,
    Add(u32),
    Exact(u32),
}

impl LeaseFilePermissions {
    pub(super) fn from_shared_repository(value: Option<&str>) -> Result<Self, OperationError> {
        let Some(value) = value else {
            return Ok(Self::Umask);
        };
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "false" | "no" | "off" | "umask" | "0" => Ok(Self::Umask),
            "true" | "yes" | "on" | "group" | "1" => Ok(Self::Add(0o660)),
            "all" | "world" | "everybody" | "2" => Ok(Self::Add(0o664)),
            numeric if numeric.len() == 4 && numeric.starts_with('0') => {
                let bits = u32::from_str_radix(&numeric[1..], 8)
                    .map_err(|_| invalid_shared_repository(value))?;
                if bits & 0o600 != 0o600 {
                    return Err(invalid_shared_repository(value));
                }
                Ok(Self::Exact(bits & 0o666))
            }
            _ => Err(invalid_shared_repository(value)),
        }
    }

    fn creation_mode(self) -> u32 {
        0o666
    }

    fn adjusted_mode(self, current: u32) -> Option<u32> {
        match self {
            Self::Umask => None,
            Self::Add(bits) => Some(current | bits),
            Self::Exact(bits) => Some(bits),
        }
    }

    #[cfg(unix)]
    fn apply(self, file: &File, path: &Path) -> Result<(), OperationError> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = file
            .metadata()
            .map_err(|error| storage_error("inspect permissions for", path, error))?;
        let current = metadata.permissions().mode() & 0o777;
        let Some(adjusted) = self.adjusted_mode(current) else {
            return Ok(());
        };
        if adjusted == current {
            return Ok(());
        }
        file.set_permissions(fs::Permissions::from_mode(adjusted))
            .map_err(|error| storage_error("apply shared permissions to", path, error))
    }

    #[cfg(not(unix))]
    fn apply(self, _file: &File, _path: &Path) -> Result<(), OperationError> {
        let _ = self.adjusted_mode(0);
        Ok(())
    }
}

fn invalid_shared_repository(value: &str) -> OperationError {
    OperationError::Storage(format!(
        "invalid core.sharedRepository value '{value}' while preparing the operation scope lease; use umask, group, all, false, or a four-digit octal mode"
    ))
}

pub(crate) struct ScopeLease {
    // Closing the independently opened file releases its lock on every platform.
    _file: File,
    _parent: File,
}

impl ScopeLease {
    pub(crate) async fn acquire(
        scope: &PinnedRequestScope,
        repo_id: &str,
    ) -> Result<Self, OperationError> {
        Self::acquire_with_permissions(scope, repo_id, LeaseFilePermissions::Umask).await
    }

    pub(super) async fn acquire_with_permissions(
        scope: &PinnedRequestScope,
        repo_id: &str,
        permissions: LeaseFilePermissions,
    ) -> Result<Self, OperationError> {
        let key = format!("{repo_id}:{}", scope.scope.storage_key());
        let gitdir_info = scope.gitdir.join("info");
        let path = gitdir_info.join("operation-v2.lock");
        let open_info = gitdir_info.clone();
        let open_path = path.clone();
        // Cancellation can leave only a short filesystem-open task, never a
        // background task waiting for or later acquiring the scope lock. This
        // does not promise a hard deadline for arbitrary filesystem I/O.
        let (file, parent) = tokio::task::spawn_blocking(move || {
            open_files_with_permissions(&open_info, &open_path, permissions)
        })
        .await
        .map_err(|error| {
            OperationError::Storage(format!(
                "cannot prepare operation scope lease '{}' for {key}: {error}",
                path.display()
            ))
        })??;
        // Git's default lock timeout is zero: one attempt, no waiting or retry.
        // There is no await between acquisition and returning its owning guard.
        file.try_lock()
            .map_err(|error| lock_error(error, &key, &path))?;
        verify_parent(&parent, &gitdir_info)?;
        Ok(Self {
            _file: file,
            _parent: parent,
        })
    }

    /// Repository-wide lease used by transitions that rewrite shared refs.
    /// Worktree leases intentionally live below this one in the lock order.
    pub(crate) async fn acquire_repository(
        scope: &PinnedRequestScope,
        repo_id: &str,
    ) -> Result<Self, OperationError> {
        let key = format!("{repo_id}:repository");
        let path = scope.storage.join("operation-v2-repository.lock");
        let open_path = path.clone();
        let parent_path = scope.storage.clone();
        let (file, parent) =
            tokio::task::spawn_blocking(move || open_repository_files(&parent_path, &open_path))
                .await
                .map_err(|error| {
                    OperationError::Storage(format!(
                        "cannot prepare repository operation lease '{}' for {key}: {error}",
                        path.display()
                    ))
                })??;
        file.try_lock()
            .map_err(|error| lock_error(error, &key, &path))?;
        verify_parent(&parent, &scope.storage)?;
        Ok(Self {
            _file: file,
            _parent: parent,
        })
    }
}

#[cfg(test)]
fn open_files(info: &Path, path: &Path) -> Result<(File, File), OperationError> {
    open_files_with_permissions(info, path, LeaseFilePermissions::Umask)
}

fn open_files_with_permissions(
    info: &Path,
    path: &Path,
    permissions: LeaseFilePermissions,
) -> Result<(File, File), OperationError> {
    match fs::create_dir(info) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(storage_error("create directory for", info, error)),
    }
    let parent = open_parent(info)?;
    let file = open_leaf(&parent, path, permissions.creation_mode())?;
    if !metadata(&file, path)?.is_file() {
        return Err(storage_error(
            "open",
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "lock must be a regular file"),
        ));
    }
    permissions.apply(&file, path)?;
    verify_parent(&parent, info)?;
    Ok((file, parent))
}

fn open_repository_files(parent_path: &Path, path: &Path) -> Result<(File, File), OperationError> {
    let parent = open_parent(parent_path)?;
    let file = {
        #[cfg(unix)]
        {
            unix::open_repository_leaf(&parent, path, 0o666)?
        }
        #[cfg(not(unix))]
        {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| storage_error("open", path, error))?
        }
    };
    if !metadata(&file, path)?.is_file() {
        return Err(storage_error(
            "open",
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "repository lease must be a regular file",
            ),
        ));
    }
    verify_parent(&parent, parent_path)?;
    Ok((file, parent))
}

fn open_parent(info: &Path) -> Result<File, OperationError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        options
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    let parent = options
        .open(info)
        .map_err(|error| storage_error("open directory for", info, error))?;
    if !metadata(&parent, info)?.is_dir() {
        return Err(storage_error(
            "open directory for",
            info,
            io::Error::new(
                io::ErrorKind::NotADirectory,
                "lease parent must be a directory",
            ),
        ));
    }
    Ok(parent)
}

#[cfg(unix)]
mod unix;

#[cfg(unix)]
use unix::open_leaf;

#[cfg(windows)]
fn open_leaf(_parent: &File, path: &Path, _mode: u32) -> Result<File, OperationError> {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
        .map_err(|error| storage_error("open", path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_leaf(_parent: &File, path: &Path, _mode: u32) -> Result<File, OperationError> {
    Err(storage_error(
        "open",
        path,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "no safe scope lease implementation for this platform",
        ),
    ))
}

fn metadata(file: &File, path: &Path) -> Result<Metadata, OperationError> {
    let metadata = file
        .metadata()
        .map_err(|error| storage_error("inspect", path, error))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(storage_error(
                "open",
                path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lease path must not be a reparse point",
                ),
            ));
        }
    }
    Ok(metadata)
}

fn verify_parent(parent: &File, info: &Path) -> Result<(), OperationError> {
    let named = open_parent(info)?;
    let held_id = entry_identity_beneath(parent, Path::new(""))
        .map_err(|error| storage_error("identify directory for", info, error))?;
    let named_id = entry_identity_beneath(&named, Path::new(""))
        .map_err(|error| storage_error("identify directory for", info, error))?;
    if held_id.key != named_id.key {
        return Err(storage_error(
            "verify directory for",
            info,
            io::Error::other("lease directory changed; stop metadata writers before retrying"),
        ));
    }
    Ok(())
}

fn storage_error(action: &str, path: &Path, error: io::Error) -> OperationError {
    OperationError::Storage(format!(
        "cannot {action} operation scope lease '{}': {error}",
        path.display()
    ))
}

fn lock_error(error: TryLockError, key: &str, path: &Path) -> OperationError {
    match error {
        TryLockError::WouldBlock => OperationError::LeaseBusy {
            scope_key: key.to_string(),
            path: path.display().to_string(),
        },
        TryLockError::Error(error) => storage_error(&format!("lock for {key}"), path, error),
    }
}

#[cfg(test)]
mod error_tests;

#[cfg(test)]
mod first_open_tests;
