//! Unix directory-anchored opening of the persistent scope lock.

use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
};

use super::{OperationError, storage_error};

const OPEN_FLAGS: libc::c_int =
    libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

pub(super) fn open_leaf(parent: &File, path: &Path, mode: u32) -> Result<File, OperationError> {
    open_named_leaf(parent, path, mode, c"operation-v2.lock")
}

pub(super) fn open_repository_leaf(
    parent: &File,
    path: &Path,
    mode: u32,
) -> Result<File, OperationError> {
    open_named_leaf(parent, path, mode, c"operation-v2-repository.lock")
}

fn open_named_leaf(
    parent: &File,
    path: &Path,
    mode: u32,
    name: &std::ffi::CStr,
) -> Result<File, OperationError> {
    let mode = mode as libc::c_int;
    // SAFETY: FFI boundary: parent is a live owned directory fd, the fixed
    // C string is NUL-terminated, and mode has C's promoted variadic type.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            OPEN_FLAGS | libc::O_CREAT | libc::O_EXCL,
            mode,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        // Only an existing entry permits the second open. In particular, never
        // retry ENOENT or recreate a leaf removed between the two attempts.
        return if error.raw_os_error() == Some(libc::EEXIST) {
            open_existing_leaf(parent, path, name)
        } else {
            Err(storage_error("create", path, error))
        };
    }
    // SAFETY: openat returned a fresh valid descriptor; ownership transfers
    // exactly once into File, which closes it on every later error path.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_existing_leaf(
    parent: &File,
    path: &Path,
    name: &std::ffi::CStr,
) -> Result<File, OperationError> {
    // SAFETY: FFI boundary: parent remains a live owned directory fd and the
    // fixed C string is NUL-terminated. OPEN_FLAGS never requests creation.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), OPEN_FLAGS) };
    if fd < 0 {
        return Err(storage_error("open", path, io::Error::last_os_error()));
    }
    // SAFETY: openat returned a fresh valid descriptor; ownership transfers
    // exactly once into File, which closes it on every later error path.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests;
