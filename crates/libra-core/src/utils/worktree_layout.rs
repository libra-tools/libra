//! Common-storage rooted working directories and their filesystem boundary.
//!
//! Path construction does not create or reserve a workspace. Callers retain
//! ownership of registry locks, exclusive leaf creation, and lifecycle records.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    internal::worktree_scope::WorktreeScope,
    utils::{beneath, util},
};

/// Resolve shared storage from the request's original repository observation.
pub fn current_common_storage() -> io::Result<PathBuf> {
    match WorktreeScope::request_scope() {
        Some(scope) => Ok(scope.storage),
        None => util::try_get_storage_path(None),
    }
}

pub fn managed_root(common: &Path) -> PathBuf {
    common.join("worktree")
}

/// Validate one portable, stable worktree name (never a filesystem path).
pub fn validate_name(name: &str) -> io::Result<()> {
    let bytes = name.as_bytes();
    let valid_first = bytes
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let valid_rest = bytes
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'));
    let stem = name.split('.').next().unwrap_or_default();
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    if bytes.len() > 64 || !valid_first || !valid_rest || name.ends_with('.') || device {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid worktree name '{name}': use 1-64 lowercase ASCII letters, digits, \
                 '.', '_' or '-', beginning with a letter or digit; trailing dots and \
                 Windows device names are not allowed (use --path for a custom location)"
            ),
        ));
    }
    Ok(())
}

pub fn linked_root(common: &Path, name: &str) -> io::Result<PathBuf> {
    validate_name(name)?;
    Ok(managed_root(common).join("linked").join(name))
}

pub fn fuse_root(common: &Path, name: &str) -> io::Result<PathBuf> {
    validate_name(name)?;
    Ok(managed_root(common).join("fuse").join(name))
}

pub fn fuse_workspace(common: &Path, name: &str) -> io::Result<PathBuf> {
    Ok(fuse_root(common, name)?.join("workspace"))
}

pub fn fuse_upper(common: &Path, name: &str) -> io::Result<PathBuf> {
    Ok(fuse_root(common, name)?.join("upper"))
}

pub fn task_attempt_root(common: &Path, run_id: Uuid, attempt_id: Uuid) -> PathBuf {
    managed_root(common)
        .join("tasks")
        .join(run_id.to_string())
        .join(attempt_id.to_string())
}

pub fn task_workspace(common: &Path, run_id: Uuid, attempt_id: Uuid) -> PathBuf {
    task_attempt_root(common, run_id, attempt_id).join("workspace")
}

pub fn checkpoint_input(common: &Path, run_id: Uuid, attempt_id: Uuid) -> PathBuf {
    task_attempt_root(common, run_id, attempt_id).join("checkpoint-input")
}

fn context(action: &str, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("cannot {action} '{}': {error}", path.display()),
    )
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Preserve the original components below common storage: canonicalizing the
/// entire destination first would hide an escaping symlink from no-follow checks.
fn managed_relative(common: &Path, path: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let common_absolute = absolute(common)?;
    let canonical = fs::canonicalize(&common_absolute)
        .map_err(|error| context("resolve common worktree storage", common, error))?;
    let absolute = absolute(path)?;
    let relative = absolute
        .strip_prefix(&common_absolute)
        .or_else(|_| absolute.strip_prefix(&canonical))
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "worktree directory '{}' is outside common storage '{}'",
                    path.display(),
                    canonical.display()
                ),
            )
        })?;
    if !relative.starts_with("worktree")
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "'{}' is not a direct path below the managed worktree container",
                path.display()
            ),
        ));
    }
    Ok((canonical, relative.to_path_buf()))
}

/// Verify every existing managed component without following symlinks. A
/// missing suffix is permitted, but this does not reserve or authorize deletion.
pub fn validate_managed_path(common: &Path, path: &Path) -> io::Result<()> {
    let (common, relative) = managed_relative(common, path)?;
    let mut parent = beneath::open_root(&common)
        .map_err(|error| context("open common worktree storage", &common, error))?;
    for component in relative.components() {
        match beneath::open_beneath(&parent, Path::new(component.as_os_str())) {
            Ok(directory) => parent = directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(context(
                    "validate managed worktree directory without following links",
                    path,
                    error,
                ));
            }
        }
    }
    Ok(())
}

/// Open an existing managed directory while retaining its filesystem identity.
pub fn open_managed_directory(common: &Path, path: &Path) -> io::Result<fs::File> {
    let (common, relative) = managed_relative(common, path)?;
    let root = beneath::open_root(&common)
        .map_err(|error| context("open common worktree storage", &common, error))?;
    beneath::open_beneath(&root, &relative).map_err(|error| {
        context(
            "open managed worktree directory without following links",
            path,
            error,
        )
    })
}

/// Idempotently create managed containers using directory-relative operations.
/// Workspace owners must separately reserve their final leaf exclusively.
pub fn ensure_managed_directory(common: &Path, path: &Path) -> io::Result<PathBuf> {
    let (common, relative) = managed_relative(common, path)?;
    let mut parent = beneath::open_root(&common)
        .map_err(|error| context("open common worktree storage", &common, error))?;
    for component in relative.components() {
        let name = component.as_os_str();
        parent = match beneath::open_beneath(&parent, Path::new(name)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_directory_at(&parent, name, false).map_err(|error| {
                    context(
                        "create managed worktree directory without following links",
                        path,
                        error,
                    )
                })?
            }
            Err(error) => {
                return Err(context(
                    "open managed worktree container without following links",
                    path,
                    error,
                ));
            }
        };
    }
    // A concurrent rename must not let the returned pathname identify a
    // symlink. Owners requiring stronger lifetime guarantees keep an open fd.
    open_managed_directory(&common, &common.join(&relative))?;
    Ok(common.join(relative))
}

/// Reserve the final managed directory exclusively and return its identity pin.
/// Existing parent containers are reused; an existing leaf is always an error.
pub fn create_managed_directory(common: &Path, path: &Path) -> io::Result<fs::File> {
    let (common, relative) = managed_relative(common, path)?;
    let parent_relative = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a workspace leaf must be below the managed worktree container",
            )
        })?;
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed workspace path has no final directory name",
        )
    })?;
    let parent_path = common.join(parent_relative);
    ensure_managed_directory(&common, &parent_path)?;
    let parent = open_managed_directory(&common, &parent_path)?;
    create_directory_at(&parent, name, true).map_err(|error| {
        context(
            "reserve managed worktree directory exclusively",
            path,
            error,
        )
    })
}

#[cfg(unix)]
fn create_directory_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    exclusive: bool,
) -> io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, io::AsRawFd},
    };
    let component = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree directory name contains NUL",
        )
    })?;
    // SAFETY: parent is a live directory descriptor and component is a single,
    // NUL-terminated child name. The subsequent no-follow open pins its identity.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) };
    if result != 0 {
        let error = io::Error::last_os_error();
        if exclusive || error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    beneath::open_beneath(parent, Path::new(name))
}

#[cfg(windows)]
fn create_directory_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    exclusive: bool,
) -> io::Result<fs::File> {
    if exclusive {
        crate::utils::windows_fs::exclusive_create_mutable_directory_at(parent, name)
    } else {
        crate::utils::windows_fs::ensure_directory_at(parent, name)
    }
}

/// Only the named default is writable inside common metadata; arbitrary
/// external destinations retain ordinary path semantics.
pub fn validate_linked_destination(
    common: &Path,
    name: &str,
    destination: &Path,
) -> io::Result<()> {
    let expected = linked_root(common, name)?;
    let canonical_common = fs::canonicalize(common)
        .map_err(|error| context("resolve common worktree storage", common, error))?;
    let target = util::canonicalize_deepest_existing(destination)?;
    let expected = util::canonicalize_deepest_existing(expected)?;
    if target.starts_with(&canonical_common) {
        if target != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "worktree destination '{}' is reserved repository metadata; use the named default or --path outside .libra",
                    destination.display()
                ),
            ));
        }
        validate_managed_path(common, destination)?;
    } else if canonical_common.starts_with(&target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worktree destination '{}' contains the repository storage; choose a separate directory",
                destination.display()
            ),
        ));
    } else if absolute(destination)?.starts_with(absolute(common)?) {
        // A syntactically internal path may resolve outside through a symlink.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worktree destination '{}' escapes repository storage through a link",
                destination.display()
            ),
        ));
    }
    Ok(())
}

/// Classify current-worktree metadata, not all shared common storage. Managed
/// worktrees themselves live below common storage and contain ordinary files.
pub fn is_worktree_metadata(path: &Path, workdir: &Path) -> bool {
    util::is_sub_path(path, workdir.join(util::ROOT_DIR))
}

/// Read bounded identity/registry metadata without following links or blocking
/// on special files. The caller supplies the trusted directory and relative path.
pub fn read_regular_metadata(
    root: &Path,
    relative: &Path,
    maximum_bytes: usize,
) -> io::Result<Vec<u8>> {
    let directory = beneath::open_root(root)?;
    read_regular_metadata_from(&directory, relative, maximum_bytes)
}

/// Descriptor-pinned form for callers retaining a registry/mount identity.
pub fn read_regular_metadata_from(
    root: &fs::File,
    relative: &Path,
    maximum_bytes: usize,
) -> io::Result<Vec<u8>> {
    use std::io::Read;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata path must contain only relative normal components",
        ));
    }
    let parent = match relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => beneath::open_beneath(root, parent)?,
        None => root.try_clone()?,
    };
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "metadata filename is missing")
    })?;
    #[cfg(unix)]
    let file = {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt,
                io::{AsRawFd, FromRawFd},
            },
        };
        let name = CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata filename contains NUL",
            )
        })?;
        // SAFETY: parent is pinned, name is one NUL-terminated component, and
        // NONBLOCK prevents an attacker-controlled FIFO from hanging discovery.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned a fresh owned descriptor.
        unsafe { fs::File::from_raw_fd(fd) }
    };
    #[cfg(windows)]
    let file = crate::utils::windows_fs::open_at(&parent, name, false)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree metadata must be a regular file",
        ));
    }
    let limit = u64::try_from(maximum_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata size limit is too large",
        )
    })?;
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree metadata exceeds its size limit",
        ));
    }
    let mut contents = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut contents)?;
    if contents.len() > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree metadata grew beyond its size limit",
        ));
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn portable_names_reject_paths_aliases_and_devices() {
        for name in ["feature-x", "0", "hello_world", "branch.v2", "lpt10"] {
            validate_name(name).unwrap();
        }
        validate_name(&"a".repeat(64)).unwrap();
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "Upper",
            "中文",
            "-x",
            "x.",
            "con",
            "con.txt",
            "nul",
            "com1",
            "com0",
            "lpt0",
            "lpt9.log",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name}");
        }
        assert!(validate_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn layout_keeps_backend_and_attempt_directories_separate() {
        let common = Path::new("/repo/.libra");
        assert_eq!(
            linked_root(common, "topic").unwrap(),
            common.join("worktree/linked/topic")
        );
        assert_eq!(
            fuse_workspace(common, "topic").unwrap(),
            common.join("worktree/fuse/topic/workspace")
        );
        assert_eq!(
            fuse_upper(common, "topic").unwrap(),
            common.join("worktree/fuse/topic/upper")
        );
        let run = Uuid::new_v4();
        let attempt = Uuid::new_v4();
        assert_eq!(
            task_workspace(common, run, attempt).parent(),
            Some(task_attempt_root(common, run, attempt).as_path())
        );
        assert_ne!(
            task_workspace(common, run, attempt),
            checkpoint_input(common, run, attempt)
        );
        assert_ne!(
            task_attempt_root(common, run, attempt),
            task_attempt_root(common, run, Uuid::new_v4())
        );
    }

    #[test]
    fn common_storage_uses_request_pin() {
        use crate::internal::worktree_scope::{RequestScope, with_request_scope_sync};
        let temp = tempdir().unwrap();
        let common = temp.path().join(".libra");
        fs::create_dir(&common).unwrap();
        fs::write(common.join(util::DATABASE), b"fixture").unwrap();
        let scope = RequestScope::try_resolve(temp.path().to_path_buf())
            .unwrap()
            .unwrap();
        with_request_scope_sync(Some(scope), || {
            assert_eq!(
                current_common_storage().unwrap(),
                common.canonicalize().unwrap()
            );
        });
    }

    #[test]
    fn managed_creation_rejects_metadata_and_traversal() {
        let temp = tempdir().unwrap();
        let common = temp.path().join(".libra");
        fs::create_dir(&common).unwrap();
        let target = linked_root(&common, "topic").unwrap();
        validate_managed_path(&common, &target).unwrap();
        ensure_managed_directory(&common, &target).unwrap();
        ensure_managed_directory(&common, &target).unwrap();
        assert!(open_managed_directory(&common, &target).is_ok());
        assert!(ensure_managed_directory(&common, &common.join("objects/new")).is_err());
        assert!(
            ensure_managed_directory(&common, &common.join("worktree/../objects/new")).is_err()
        );
        assert!(!common.join("objects").exists());
    }

    #[test]
    fn exclusive_workspace_creation_never_adopts_existing_data() {
        let temp = tempdir().unwrap();
        let common = temp.path().join(".libra");
        fs::create_dir(&common).unwrap();
        let target = linked_root(&common, "topic").unwrap();
        let owned = create_managed_directory(&common, &target).unwrap();
        assert!(owned.metadata().unwrap().is_dir());
        fs::write(target.join("keep"), b"user data").unwrap();
        assert_eq!(
            create_managed_directory(&common, &target)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"user data");
    }

    #[cfg(unix)]
    #[test]
    fn metadata_read_rejects_fifo_symlinks_and_oversized_identity() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::symlink},
        };
        let temp = tempdir().unwrap();
        let fifo = temp.path().join("identity");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c is a valid NUL-terminated fixture path.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_regular_metadata(temp.path(), Path::new("identity"), 128)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&fifo).unwrap();
        symlink(temp.path().join("missing"), &fifo).unwrap();
        assert!(read_regular_metadata(temp.path(), Path::new("identity"), 128).is_err());
        fs::remove_file(&fifo).unwrap();
        fs::write(&fifo, vec![b'x'; 129]).unwrap();
        assert!(read_regular_metadata(temp.path(), Path::new("identity"), 128).is_err());
        fs::write(&fifo, b"valid-id").unwrap();
        assert_eq!(
            read_regular_metadata(temp.path(), Path::new("identity"), 128).unwrap(),
            b"valid-id"
        );
    }

    #[test]
    fn destination_and_metadata_classification_preserve_inner_workfiles() {
        let temp = tempdir().unwrap();
        let common = temp.path().join(".libra");
        fs::create_dir(&common).unwrap();
        let target = linked_root(&common, "topic").unwrap();
        validate_linked_destination(&common, "topic", &target).unwrap();
        validate_linked_destination(&common, "topic", &temp.path().join("external")).unwrap();
        assert!(validate_linked_destination(&common, "topic", &common.join("objects")).is_err());
        assert!(validate_linked_destination(&common, "topic", temp.path()).is_err());
        assert!(validate_linked_destination(&common, "other", &target).is_err());
        assert!(!is_worktree_metadata(&target.join("file.txt"), &target));
        assert!(is_worktree_metadata(&target.join(".libra/index"), &target));
        assert!(is_worktree_metadata(&target.join("file.txt"), temp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn managed_paths_reject_symlinks_including_dangling_ones() {
        use std::os::unix::fs::symlink;
        for dangling in [false, true] {
            let temp = tempdir().unwrap();
            let common = temp.path().join(".libra");
            let outside = temp.path().join("outside");
            fs::create_dir(&common).unwrap();
            if !dangling {
                fs::create_dir(&outside).unwrap();
            }
            symlink(&outside, common.join("worktree")).unwrap();
            let target = linked_root(&common, "topic").unwrap();
            assert!(validate_managed_path(&common, &target).is_err());
            assert!(ensure_managed_directory(&common, &target).is_err());
            assert!(validate_linked_destination(&common, "topic", &target).is_err());
            assert!(!outside.join("linked").exists());
        }
    }
}
