//! Helpers to read or write compressed git objects on disk, returning raw payloads and computing their object hashes.

#[cfg(target_os = "linux")]
use std::time::SystemTime;
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use flate2::read::ZlibDecoder;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
    utils::HashAlgorithm,
};

use crate::utils::atomic_write::{self, ensure_dir_exists};

const STALE_LOOSE_OBJECT_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(target_os = "linux")]
const MAX_STALE_LOOSE_OBJECT_TEMPS_PER_WRITE: usize = 64;

/// macOS exposes a few fixed system paths through root-level aliases.  The
/// descriptor-relative walkers below must not follow repository-controlled
/// symlinks, but rejecting these OS-owned aliases would make an ordinary
/// repository under `/tmp` unusable (`/tmp` is `/private/tmp` on macOS).
#[cfg(target_os = "macos")]
fn normalize_macos_system_directory_alias(path: &Path) -> std::path::PathBuf {
    for (alias, canonical) in [
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/var"), Path::new("/private/var")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return canonical.join(suffix);
        }
    }
    path.to_path_buf()
}

#[cfg(target_os = "linux")]
fn loose_object_temp_name_is_valid(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((oid, suffix)) = rest.split_once(".tmp-") else {
        return false;
    };
    if !matches!(oid.len(), 40 | 64)
        || !oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    let Some((pid, generation)) = suffix.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && uuid::Uuid::parse_str(generation).is_ok()
}

#[cfg(not(unix))]
fn prepare_loose_object_temp_dir(
    path: &Path,
    sync_data: bool,
) -> std::io::Result<std::path::PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "loose-object temp directory has no parent: {}",
                path.display()
            ),
        )
    })?;
    ensure_dir_exists(parent, sync_data)?;
    match fs::create_dir(path) {
        Ok(()) => {
            #[cfg(unix)]
            sync_loose_object_file(&fs::File::open(parent)?, sync_data)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "loose-object temp path is not a real directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "loose-object temp path is a Windows reparse point: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
pub(crate) fn open_directory_tree_no_follow(path: &Path) -> std::io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
        path::Component,
    };

    // Keep the no-follow walk strict for every repository-controlled
    // component, but normalize fixed macOS system aliases first.
    #[cfg(target_os = "macos")]
    let path = normalize_macos_system_directory_alias(path);

    let start = if path.is_absolute() { "/" } else { "." };
    let start = CString::new(start).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid directory root")
    })?;
    // SAFETY: `start` is NUL-terminated and a successful descriptor is
    // immediately owned.
    let fd = unsafe {
        libc::open(
            start.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor returned by `open`.
    let mut current = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory path contains a non-normal component",
                ));
            }
        };
        let name = CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory component contains NUL",
            )
        })?;
        // SAFETY: current is a live directory fd and name is NUL-terminated.
        let next = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `next` is a fresh descriptor returned by `openat`.
        current = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(next) };
    }
    Ok(current)
}

/// Open a direct child directory from a descriptor-pinned parent without
/// following a symlink. Diagnostic readers use this after enumerating names
/// so an attacker cannot replace a listed entry before its contents are read.
#[cfg(unix)]
pub(crate) fn open_directory_at_no_follow(
    parent: &fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory name contains NUL",
        )
    })?;
    // SAFETY: parent is a live directory descriptor and name is a
    // NUL-terminated single path component.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is freshly returned by openat and transferred once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory entry changed type while opening",
        ));
    }
    Ok(file)
}

/// Open a direct child regular file from a descriptor-pinned parent without
/// following a symlink. `O_NONBLOCK` prevents a hostile FIFO from stalling a
/// foreground diagnostic if an OS/filesystem does not reject it at open.
#[cfg(unix)]
pub(crate) fn open_regular_file_at_no_follow(
    parent: &fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file name contains NUL")
    })?;
    // SAFETY: parent is a live directory descriptor and name is a
    // NUL-terminated single path component.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is freshly returned by openat and transferred once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file entry is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
pub(crate) fn open_directory_tree_no_follow(_path: &Path) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure descriptor-relative reads are unavailable on this platform",
    ))
}

#[cfg(not(unix))]
pub(crate) fn open_directory_at_no_follow(
    _parent: &fs::File,
    _name: &std::ffi::OsStr,
) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure descriptor-relative reads are unavailable on this platform",
    ))
}

#[cfg(not(unix))]
pub(crate) fn open_regular_file_at_no_follow(
    _parent: &fs::File,
    _name: &std::ffi::OsStr,
) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure descriptor-relative reads are unavailable on this platform",
    ))
}

#[cfg(unix)]
fn open_or_create_directory_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    sync_data: bool,
) -> std::io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "loose-object directory component contains NUL",
        )
    })?;
    let open = || {
        // SAFETY: parent is live and name is NUL-terminated.
        unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        }
    };
    let mut fd = open();
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error);
        }
        // SAFETY: parent is live and name is NUL-terminated.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
        if created < 0 {
            let create_error = std::io::Error::last_os_error();
            if create_error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(create_error);
            }
        } else {
            sync_loose_object_file(parent, sync_data)?;
        }
        fd = open();
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is fresh from openat.
    Ok(unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_or_create_directory_tree_no_follow(
    path: &Path,
    sync_data: bool,
) -> std::io::Result<fs::File> {
    use std::path::Component;

    #[cfg(target_os = "macos")]
    let path = normalize_macos_system_directory_alias(path);

    let mut current = open_directory_tree_no_follow(if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    })?;
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "loose-object directory path contains a non-normal component",
                ));
            }
        };
        current = open_or_create_directory_at(&current, name, sync_data)?;
    }
    Ok(current)
}

#[cfg(unix)]
struct LooseObjectDirectories {
    temporary: fs::File,
    shard: fs::File,
}

#[cfg(unix)]
fn prepare_loose_object_directories(
    git_dir: &Path,
    shard_name: &str,
    sync_data: bool,
) -> std::io::Result<LooseObjectDirectories> {
    let git_dir = open_or_create_directory_tree_no_follow(git_dir, sync_data)?;
    let objects =
        open_or_create_directory_at(&git_dir, std::ffi::OsStr::new("objects"), sync_data)?;
    let shard = open_or_create_directory_at(&objects, std::ffi::OsStr::new(shard_name), sync_data)?;
    let info = open_or_create_directory_at(&objects, std::ffi::OsStr::new("info"), sync_data)?;
    let temporary =
        open_or_create_directory_at(&info, std::ffi::OsStr::new("libra-tmp"), sync_data)?;
    Ok(LooseObjectDirectories { temporary, shard })
}

#[cfg(unix)]
struct LooseObjectTempFile {
    directory: fs::File,
    name: std::ffi::CString,
    active: bool,
}

#[cfg(unix)]
impl LooseObjectTempFile {
    fn create(directory: &fs::File, name: &str) -> std::io::Result<(fs::File, Self)> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let name = std::ffi::CString::new(name).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "loose-object temp name contains NUL",
            )
        })?;
        // SAFETY: the directory descriptor is live, the name is
        // NUL-terminated, and a successful fd is immediately owned.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o666,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor returned by `openat`.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        Ok((
            file,
            Self {
                directory: directory.try_clone()?,
                name,
                active: true,
            },
        ))
    }

    fn publish(&self, target_directory: &fs::File, target_name: &str) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;

        let target = std::ffi::CString::new(target_name).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "loose-object target name contains NUL",
            )
        })?;
        // SAFETY: both names are NUL-terminated and the source is resolved
        // relative to the pinned temp-directory descriptor.
        let result = unsafe {
            libc::linkat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                target_directory.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn remove(&mut self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;

        if !self.active {
            return Ok(());
        }
        // SAFETY: the name is NUL-terminated and is resolved only beneath
        // the pinned directory descriptor.
        let result = unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        self.active = false;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for LooseObjectTempFile {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[cfg(not(unix))]
struct LooseObjectTempFile {
    path: std::path::PathBuf,
    active: bool,
}

#[cfg(not(unix))]
impl LooseObjectTempFile {
    fn create(directory: &Path, name: &str) -> std::io::Result<(fs::File, Self)> {
        let path = directory.join(name);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((file, Self { path, active: true }))
    }

    fn publish(&self, target: &Path) -> std::io::Result<()> {
        fs::hard_link(&self.path, target)
    }

    fn remove(&mut self) -> std::io::Result<()> {
        if self.active {
            match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.active = false;
        }
        Ok(())
    }
}

#[cfg(not(unix))]
impl Drop for LooseObjectTempFile {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// Helper function to read and decompress a git object from the object database.
pub fn read_git_object(git_dir: &Path, hash: &ObjectHash) -> Result<Vec<u8>, GitError> {
    Ok(read_git_object_with_type(git_dir, hash)?.1)
}

/// Read a loose object's declared Git type together with its uncompressed
/// payload. Reachability walkers need the type to traverse commit, tree, and
/// annotated-tag roots without assuming every ref points directly to a commit.
pub(crate) fn read_git_object_with_type(
    git_dir: &Path,
    hash: &ObjectHash,
) -> Result<(String, Vec<u8>), GitError> {
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);

    let file = fs::File::open(object_path)?;
    let mut decoder = ZlibDecoder::new(file);
    let mut buffer = Vec::new();
    decoder.read_to_end(&mut buffer)?;
    decode_and_validate_object_buffer(hash, buffer)
}

fn decode_and_validate_object_buffer(
    expected_hash: &ObjectHash,
    buffer: Vec<u8>,
) -> Result<(String, Vec<u8>), GitError> {
    let actual_hash = ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &buffer);
    if &actual_hash != expected_hash {
        return Err(GitError::InvalidObjectInfo(format!(
            "loose object content hashes to {actual_hash}, expected {expected_hash}"
        )));
    }

    // The buffer now contains "<type> <size>\0<content>", where <type> is the git object type (e.g., commit, tree, blob, tag)
    // Strip the header (which contains the object type and size) to obtain only the object content.
    if let Some(header_end) = buffer.iter().position(|&b| b == 0) {
        let header = std::str::from_utf8(&buffer[..header_end]).map_err(|error| {
            GitError::InvalidObjectInfo(format!("object header is not UTF-8: {error}"))
        })?;
        let (object_type, declared_size) = header.split_once(' ').ok_or_else(|| {
            GitError::InvalidObjectInfo("object header is missing type/size".to_string())
        })?;
        let declared_size = declared_size.parse::<usize>().map_err(|error| {
            GitError::InvalidObjectInfo(format!("object header has invalid size: {error}"))
        })?;
        let payload = buffer[header_end + 1..].to_vec();
        if payload.len() != declared_size {
            return Err(GitError::InvalidObjectInfo(format!(
                "object payload length {} does not match declared size {declared_size}",
                payload.len()
            )));
        }
        Ok((object_type.to_string(), payload))
    } else {
        Err(GitError::InvalidObjectInfo(
            "Could not find object header terminator".to_string(),
        ))
    }
}

/// Read a git object but decode at most `max_content_bytes` of content,
/// returning `(content, truncated)`. Unlike [`read_git_object`], this never
/// decompresses the whole blob into memory — the zlib stream is read
/// through a bounded reader, so a corrupt/hostile object whose inflated
/// size dwarfs the cap cannot force an unbounded allocation (AG-24a raw
/// export must respect `agent.max_transcript_read_bytes`).
pub fn read_git_object_bounded(
    git_dir: &Path,
    hash: &ObjectHash,
    max_content_bytes: u64,
) -> Result<(Vec<u8>, bool), GitError> {
    // Hard cap on the "<type> <size>\0" header. A legitimate git header is
    // well under this (type is a short word, size is decimal digits); more
    // means a corrupt object. Parsing the header separately — rather than
    // assuming it fits within a fixed content-slack — is what makes
    // truncation detection independent of header length (codex review R4).
    const HEADER_MAX: usize = 64;
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);

    let file = fs::File::open(object_path)?;
    let mut decoder = ZlibDecoder::new(file);

    // 1. Consume the header up to (and including) the NUL terminator,
    //    byte-by-byte under the hard cap. The header bytes themselves are
    //    discarded — callers only want the content.
    let mut byte = [0u8; 1];
    let mut header_len = 0usize;
    loop {
        let n = decoder.read(&mut byte)?;
        if n == 0 {
            return Err(GitError::InvalidObjectInfo(
                "object stream ended before the header terminator".to_string(),
            ));
        }
        if byte[0] == 0 {
            break;
        }
        header_len += 1;
        if header_len > HEADER_MAX {
            return Err(GitError::InvalidObjectInfo(
                "object header exceeds the maximum size (corrupt object)".to_string(),
            ));
        }
    }

    // 2. Read exactly `max_content_bytes + 1` content bytes. Observing the
    //    extra byte proves there is more content than the cap, so
    //    truncation is detected by content length alone — no dependence on
    //    how large the header was.
    let read_limit = max_content_bytes.saturating_add(1);
    let mut content = Vec::new();
    decoder.take(read_limit).read_to_end(&mut content)?;
    let truncated = content.len() as u64 > max_content_bytes;
    if truncated {
        content.truncate(max_content_bytes as usize);
    }
    Ok((content, truncated))
}

/// Read one loose object under a strict inflated-payload cap and validate its
/// declared type/size plus full content-addressed identity. Unlike the
/// truncating export reader above, callers either receive the complete,
/// verified object or an error; the decoder never allocates beyond
/// `max_content_bytes + 1` for a hostile compression stream.
pub(crate) fn read_git_object_bounded_validated(
    git_dir: &Path,
    hash: &ObjectHash,
    max_content_bytes: u64,
) -> Result<(String, Vec<u8>), GitError> {
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);
    let file = fs::File::open(object_path)?;
    read_git_object_bounded_validated_file(file, hash, max_content_bytes)
}

/// Validate and decode a bounded loose object that was already opened by a
/// caller. Keeping the descriptor lets a security-sensitive caller perform
/// its own no-follow directory walk and prevents a pathname swap between the
/// ownership check and decompression.
pub(crate) fn read_git_object_bounded_validated_file(
    file: fs::File,
    hash: &ObjectHash,
    max_content_bytes: u64,
) -> Result<(String, Vec<u8>), GitError> {
    const HEADER_MAX: usize = 64;
    let mut decoder = ZlibDecoder::new(file);
    let mut header = Vec::with_capacity(HEADER_MAX);
    let mut byte = [0_u8; 1];
    loop {
        let read = decoder.read(&mut byte)?;
        if read == 0 {
            return Err(GitError::InvalidObjectInfo(
                "object stream ended before the header terminator".to_string(),
            ));
        }
        if byte[0] == 0 {
            break;
        }
        if header.len() == HEADER_MAX {
            return Err(GitError::InvalidObjectInfo(
                "object header exceeds the maximum size (corrupt object)".to_string(),
            ));
        }
        header.push(byte[0]);
    }
    let header_text = std::str::from_utf8(&header).map_err(|error| {
        GitError::InvalidObjectInfo(format!("object header is not UTF-8: {error}"))
    })?;
    let (object_type, declared_size) = header_text.split_once(' ').ok_or_else(|| {
        GitError::InvalidObjectInfo("object header is missing type/size".to_string())
    })?;
    let declared_size = declared_size.parse::<u64>().map_err(|error| {
        GitError::InvalidObjectInfo(format!("object header has invalid size: {error}"))
    })?;
    if declared_size > max_content_bytes {
        return Err(GitError::InvalidObjectInfo(format!(
            "object declares {declared_size} bytes, exceeding the {max_content_bytes}-byte checkpoint read limit"
        )));
    }
    let declared_size_usize = usize::try_from(declared_size).map_err(|_| {
        GitError::InvalidObjectInfo("object size exceeds this platform".to_string())
    })?;
    let mut content = Vec::new();
    content
        .try_reserve_exact(declared_size_usize.saturating_add(1))
        .map_err(|error| {
            GitError::InvalidObjectInfo(format!("reserve bounded object buffer: {error}"))
        })?;
    decoder
        .take(declared_size.saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() != declared_size_usize {
        return Err(GitError::InvalidObjectInfo(format!(
            "object payload length {} does not match declared size {declared_size}",
            content.len()
        )));
    }
    let actual_hash = git_object_hash(object_type, &content);
    if &actual_hash != hash {
        return Err(GitError::InvalidObjectInfo(format!(
            "loose object content hashes to {actual_hash}, expected {hash}"
        )));
    }
    Ok((object_type.to_string(), content))
}

/// Stream-validate an already-opened loose object without materializing its
/// payload. This is for metadata-only callers that still need an
/// object-index size they can trust: the declared loose-object header is
/// bounded, the payload is streamed under `max_content_bytes`, and the
/// complete canonical object hash must match `hash` before the size is
/// returned.
///
/// Keeping the caller's descriptor is important for security-sensitive
/// diagnostics: no pathname is re-opened between a no-follow ownership check
/// and the integrity check, and a transcript never becomes an in-memory
/// buffer just because doctor needs to repair its index row.
pub(crate) fn validate_git_object_streaming_file(
    file: fs::File,
    hash: &ObjectHash,
    max_content_bytes: u64,
) -> Result<(String, u64), GitError> {
    const HEADER_MAX: usize = 64;
    const STREAM_BUFFER_BYTES: usize = 32 * 1024;

    let mut decoder = ZlibDecoder::new(file);
    let mut header = Vec::with_capacity(HEADER_MAX);
    let mut byte = [0_u8; 1];
    loop {
        let read = decoder.read(&mut byte)?;
        if read == 0 {
            return Err(GitError::InvalidObjectInfo(
                "object stream ended before the header terminator".to_string(),
            ));
        }
        if byte[0] == 0 {
            break;
        }
        if header.len() == HEADER_MAX {
            return Err(GitError::InvalidObjectInfo(
                "object header exceeds the maximum size (corrupt object)".to_string(),
            ));
        }
        header.push(byte[0]);
    }
    let header_text = std::str::from_utf8(&header).map_err(|error| {
        GitError::InvalidObjectInfo(format!("object header is not UTF-8: {error}"))
    })?;
    let (object_type, declared_size) = header_text.split_once(' ').ok_or_else(|| {
        GitError::InvalidObjectInfo("object header is missing type/size".to_string())
    })?;
    let declared_size = declared_size.parse::<u64>().map_err(|error| {
        GitError::InvalidObjectInfo(format!("object header has invalid size: {error}"))
    })?;
    if declared_size > max_content_bytes {
        return Err(GitError::InvalidObjectInfo(format!(
            "object declares {declared_size} bytes, exceeding the {max_content_bytes}-byte streaming validation limit"
        )));
    }

    let mut hasher = HashAlgorithm::new_for_kind(hash.kind());
    hasher.update(&header);
    hasher.update(&[0]);
    let mut remaining = declared_size;
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    while remaining > 0 {
        let to_read = usize::try_from(remaining.min(STREAM_BUFFER_BYTES as u64)).map_err(|_| {
            GitError::InvalidObjectInfo("object size exceeds this platform".to_string())
        })?;
        let read = decoder.read(&mut buffer[..to_read])?;
        if read == 0 {
            return Err(GitError::InvalidObjectInfo(
                "object payload ended before its declared size".to_string(),
            ));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    if decoder.read(&mut extra)? != 0 {
        return Err(GitError::InvalidObjectInfo(
            "object payload exceeds its declared size".to_string(),
        ));
    }
    let actual_hash = hasher.finalize_object_hash();
    if &actual_hash != hash {
        return Err(GitError::InvalidObjectInfo(format!(
            "loose object content hashes to {actual_hash}, expected {hash}"
        )));
    }
    Ok((object_type.to_string(), declared_size))
}

/// Open a local loose object without following any repository-controlled
/// directory or final-file symlink. This is intentionally separate from the
/// ordinary object reader: most Git operations retain compatibility with
/// existing storage layouts, while diagnostics that may later repair/index an
/// object need a provenance-tight descriptor.
#[cfg(unix)]
pub(crate) fn open_local_loose_object_no_follow(
    git_dir: &Path,
    hash: &ObjectHash,
) -> std::io::Result<fs::File> {
    use std::{
        ffi::{CString, OsStr},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let object_root = open_directory_tree_no_follow(&git_dir.join("objects"))?;
    let hash = hash.to_string();
    // INVARIANT: ObjectHash formatting is lowercase ASCII hex, so both
    // descriptor-relative path components are fixed safe names.
    let shard = CString::new(OsStr::new(&hash[..2]).as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid loose-object shard",
        )
    })?;
    // SAFETY: object_root is a live directory descriptor and shard is a
    // NUL-terminated component with no slash or traversal syntax.
    let shard_fd = unsafe {
        libc::openat(
            object_root.as_raw_fd(),
            shard.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if shard_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: shard_fd is freshly returned by openat and transferred once.
    let shard = unsafe { fs::File::from_raw_fd(shard_fd) };
    let leaf = CString::new(OsStr::new(&hash[2..]).as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid loose-object name",
        )
    })?;
    // O_NONBLOCK ensures a malicious replacement cannot make a diagnostic
    // block on a FIFO even if the filesystem ignores O_NOFOLLOW semantics.
    // SAFETY: shard is live and leaf is a NUL-terminated safe component.
    let object_fd = unsafe {
        libc::openat(
            shard.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if object_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: object_fd is freshly returned by openat and transferred once.
    let file = unsafe { fs::File::from_raw_fd(object_fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "loose object is not a regular file",
        ));
    }
    Ok(file)
}

/// There is no cross-platform descriptor-relative no-follow primitive in the
/// standard library. A doctor scan must fail closed rather than use a
/// path-check-then-open sequence that a local attacker can race.
#[cfg(not(unix))]
pub(crate) fn open_local_loose_object_no_follow(
    _git_dir: &Path,
    _hash: &ObjectHash,
) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure loose-object reads are unavailable on this platform",
    ))
}

/// Helper function to write a git object to the object database.
pub fn write_git_object(
    git_dir: &Path,
    object_type: &str,
    data: &[u8],
) -> Result<ObjectHash, GitError> {
    let header = format!("{} {}\0", object_type, data.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(data);
    let hash = git_object_hash(object_type, data);
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);
    if object_path.exists() {
        validate_existing_object(&object_path, &content)?;
        return Ok(hash);
    }

    // General object writers need atomic replacement, but not checkpoint
    // ownership attribution. Use the portable rename-based stream so callers
    // such as stash/review do not acquire a hard-link filesystem requirement.
    // Concurrent writers publish identical content-addressed bytes.
    let sync_data = atomic_write::sync_data_enabled();
    let parent = object_path.parent().ok_or_else(|| {
        GitError::InvalidObjectInfo("loose-object path has no parent directory".to_string())
    })?;
    ensure_dir_exists(parent, sync_data)?;
    // General objects stage beside their destination and preserve the legacy
    // `File::create` permission contract (`0666 & umask`). Private atomic
    // state keeps StreamingAtomicFile's restrictive 0600 default.
    #[cfg(unix)]
    let temporary = {
        use std::os::unix::fs::PermissionsExt;

        crate::utils::atomic_stream::StreamingAtomicFile::new_in_with_permissions(
            parent,
            sync_data,
            fs::Permissions::from_mode(0o666),
        )?
    };
    #[cfg(not(unix))]
    let temporary = crate::utils::atomic_stream::StreamingAtomicFile::new_in(parent, sync_data)?;
    let mut encoder = flate2::write::ZlibEncoder::new(temporary, flate2::Compression::default());
    encoder.write_all(&content)?;
    let temporary = encoder.finish()?;
    temporary.persist(&object_path)?;
    Ok(hash)
}

/// Compute the object id for the exact loose-object payload that
/// [`write_git_object_with_status`] would write, without touching disk.
/// Writers that need crash-durable ownership can persist this id before the
/// corresponding object becomes visible in the shared object database.
pub(crate) fn git_object_hash(object_type: &str, data: &[u8]) -> ObjectHash {
    git_object_hash_for_kind(object_type, data, git_internal::hash::get_hash_kind())
}

fn git_object_hash_for_kind(object_type: &str, data: &[u8], kind: HashKind) -> ObjectHash {
    let header = format!("{} {}\0", object_type, data.len());
    let mut hasher = HashAlgorithm::new_for_kind(kind);
    hasher.update(header.as_bytes());
    hasher.update(data);
    hasher.finalize_object_hash()
}

/// Write one loose object and report whether this call created its payload.
/// The `create_new` open closes the existence-check race between concurrent
/// checkpoint writers; an already-present content-addressed object is reused.
pub(crate) fn write_git_object_with_status(
    git_dir: &Path,
    object_type: &str,
    data: &[u8],
) -> Result<(ObjectHash, bool), GitError> {
    write_git_object_with_status_inner(
        git_dir,
        object_type,
        data,
        atomic_write::sync_data_enabled(),
    )
}

fn write_git_object_with_status_inner(
    git_dir: &Path,
    object_type: &str,
    data: &[u8],
    sync_data: bool,
) -> Result<(ObjectHash, bool), GitError> {
    write_git_object_with_status_compressed(
        git_dir,
        object_type,
        data,
        sync_data,
        flate2::Compression::default(),
        git_internal::hash::get_hash_kind(),
    )
}

/// Archive a verified opaque payload using zlib stored blocks.
/// Ciphertext compression is deliberately skipped to keep hashing and writing
/// within the capture CPU budget; the canonical loose-object format is retained.
/// Provenance is checked by the reasoning writer before this blob-only entry
/// or its private helper is called. Existing objects are validated and reused
/// unchanged, regardless of the compression used when they were first written.
pub(crate) fn write_opaque_blob_with_status(
    git_dir: &Path,
    data: &[u8],
    kind: HashKind,
) -> Result<(ObjectHash, bool), GitError> {
    write_git_object_with_status_compressed(
        git_dir,
        "blob",
        data,
        atomic_write::sync_data_enabled(),
        flate2::Compression::none(),
        kind,
    )
}

fn write_git_object_with_status_compressed(
    git_dir: &Path,
    object_type: &str,
    data: &[u8],
    sync_data: bool,
    compression: flate2::Compression,
    kind: HashKind,
) -> Result<(ObjectHash, bool), GitError> {
    let header = format!("{} {}\0", object_type, data.len());
    let canonical_bytes = || {
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(data);
        bytes
    };
    // Ordinary compression keeps its original single-buffer write. A new
    // opaque blob needs no extra full-payload copy; build that comparison
    // buffer only when validating an existing or concurrently published file.
    let mut content = (compression.level() != 0).then(canonical_bytes);
    let hash = git_object_hash_for_kind(object_type, data, kind);
    let hash_str = hash.to_string();

    #[cfg(not(unix))]
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);

    // Temp + no-clobber publication always provides process-crash atomicity.
    // Power-loss durability follows the repository-wide bulk-object contract
    // and is enabled only by --sync-data / LIBRA_SYNC_DATA.
    #[cfg(unix)]
    let directories = prepare_loose_object_directories(git_dir, &hash_str[..2], sync_data)
        .map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "prepare loose-object directories under '{}': {error}",
                git_dir.display()
            ))
        })?;
    #[cfg(not(unix))]
    let temporary_dir = {
        // INVARIANT: `object_path` is built from three fixed components, so
        // it always has a destination parent on non-Unix platforms.
        let parent = object_path
            .parent()
            .expect("loose-object path always has a parent directory");
        ensure_dir_exists(parent, sync_data)?;
        prepare_loose_object_temp_dir(&git_dir.join("objects/info/libra-tmp"), sync_data)?
    };
    #[cfg(unix)]
    scavenge_stale_loose_object_temps_in_dir(
        &directories.temporary,
        STALE_LOOSE_OBJECT_TEMP_AGE,
        sync_data,
    )?;
    #[cfg(not(unix))]
    scavenge_stale_loose_object_temps_in_dir(
        &temporary_dir,
        STALE_LOOSE_OBJECT_TEMP_AGE,
        sync_data,
    )?;
    #[cfg(unix)]
    if let Some(existing) = open_existing_loose_object_at(&directories.shard, &hash_str[2..])? {
        validate_existing_object_file(existing, content.get_or_insert_with(canonical_bytes))?;
        return Ok((hash, false));
    }
    #[cfg(not(unix))]
    if object_path.exists() {
        validate_existing_object(&object_path, content.get_or_insert_with(canonical_bytes))?;
        return Ok((hash, false));
    }

    let temporary_name = format!(
        ".{}.tmp-{}-{}",
        hash_str,
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    #[cfg(unix)]
    let (file, mut temporary) =
        LooseObjectTempFile::create(&directories.temporary, &temporary_name)?;
    #[cfg(not(unix))]
    let (file, mut temporary) = LooseObjectTempFile::create(&temporary_dir, &temporary_name)?;
    let mut encoder = flate2::write::ZlibEncoder::new(file, compression);
    if let Some(content) = &content {
        encoder.write_all(content)?;
    } else {
        encoder.write_all(header.as_bytes())?;
        encoder.write_all(data)?;
    }
    let completed = encoder.finish()?;
    if let Err(error) = sync_loose_object_file(&completed, sync_data) {
        return Err(error.into());
    }

    #[cfg(unix)]
    let published = temporary.publish(&directories.shard, &hash_str[2..]);
    #[cfg(not(unix))]
    let published = temporary.publish(&object_path);
    let created = match published {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(unix)]
            validate_existing_object_file(
                open_existing_loose_object_at(&directories.shard, &hash_str[2..])?.ok_or_else(
                    || {
                        GitError::InvalidObjectInfo(
                            "concurrent loose object disappeared before validation".to_string(),
                        )
                    },
                )?,
                content.get_or_insert_with(canonical_bytes),
            )?;
            #[cfg(not(unix))]
            validate_existing_object(&object_path, content.get_or_insert_with(canonical_bytes))?;
            false
        }
        Err(error) => return Err(error.into()),
    };
    temporary.remove()?;
    #[cfg(unix)]
    sync_loose_object_file(&directories.shard, sync_data)?;

    Ok((hash, created))
}

/// Inspect at most a fixed number of entries in Libra's private loose-object
/// temp directory. Only exact Libra temp names are disposable. Unix pins the
/// directory no-follow and unlinks relative to that descriptor so a symlink
/// or concurrent path swap can never redirect cleanup outside the object DB.
#[cfg(all(unix, target_os = "linux"))]
fn scavenge_stale_loose_object_temps_in_dir(
    directory: &fs::File,
    stale_age: Duration,
    sync_data: bool,
) -> Result<(), GitError> {
    use std::{ffi::CString, os::fd::AsRawFd};

    let pinned_path = Path::new("/proc/self/fd").join(directory.as_raw_fd().to_string());
    let now = SystemTime::now();
    let mut removed = false;
    for entry in fs::read_dir(pinned_path)?.take(MAX_STALE_LOOSE_OBJECT_TEMPS_PER_WRITE) {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !loose_object_temp_name_is_valid(&name) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() < stale_age {
            continue;
        }
        let name = CString::new(name.as_bytes()).map_err(|_| {
            GitError::InvalidObjectInfo("loose-object temp name contains NUL".to_string())
        })?;
        // SAFETY: the name is NUL-terminated and is resolved relative to
        // the pinned directory descriptor.
        let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            removed = true;
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    if removed {
        sync_loose_object_file(directory, sync_data)?;
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn scavenge_stale_loose_object_temps_in_dir(
    directory: &fs::File,
    stale_age: Duration,
    sync_data: bool,
) -> Result<(), GitError> {
    // macOS exposes an open directory descriptor through `/dev/fd/<fd>` as a
    // special file, not an enumerable directory. Calling `read_dir` on that
    // path therefore returns ENOTDIR and used to block every checkpoint blob
    // write. Keep the write path available; the owning process still removes
    // its own temporary file, while a later maintenance pass can handle old
    // crash remnants with a platform-native descriptor iterator.
    let _ = (directory, stale_age, sync_data);
    Ok(())
}

#[cfg(not(unix))]
fn scavenge_stale_loose_object_temps_in_dir(
    parent: &Path,
    stale_age: Duration,
    sync_data: bool,
) -> Result<(), GitError> {
    // Windows reparse points were rejected by
    // `prepare_loose_object_temp_dir`. Avoid path-relative crash-remnant
    // deletion on platforms without `unlinkat`; current-process guards still
    // remove their own exact temp files.
    let _ = (parent, stale_age, sync_data);
    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_LOOSE_OBJECT_SYNC_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn sync_loose_object_file(file: &fs::File, enabled: bool) -> std::io::Result<()> {
    if !enabled {
        return Ok(());
    }
    #[cfg(test)]
    TEST_LOOSE_OBJECT_SYNC_CALLS.with(|calls| calls.set(calls.get() + 1));
    file.sync_all()
}

#[cfg(test)]
fn reset_test_loose_object_sync_calls() {
    TEST_LOOSE_OBJECT_SYNC_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
fn test_loose_object_sync_calls() -> usize {
    TEST_LOOSE_OBJECT_SYNC_CALLS.with(std::cell::Cell::get)
}

fn validate_existing_object(path: &Path, expected: &[u8]) -> Result<(), GitError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(GitError::InvalidObjectInfo(format!(
            "existing loose-object path '{}' is not a regular file",
            path.display()
        )));
    }
    validate_existing_object_reader(
        fs::File::open(path)?,
        expected,
        &format!("'{}'", path.display()),
    )
}

fn validate_existing_object_reader(
    file: fs::File,
    expected: &[u8],
    label: &str,
) -> Result<(), GitError> {
    let storage_len = file.metadata()?.len();
    let mut decoder = ZlibDecoder::new(file);
    let mut actual = Vec::new();
    Read::by_ref(&mut decoder)
        .take((expected.len() as u64).saturating_add(1))
        .read_to_end(&mut actual)?;
    let mut extra = [0_u8; 1];
    let inflated_extra = decoder.read(&mut extra)? != 0;
    if actual != expected || inflated_extra || decoder.total_in() != storage_len {
        return Err(GitError::InvalidObjectInfo(format!(
            "existing loose object {label} is corrupt or does not match its object id"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn open_existing_loose_object_at(
    directory: &fs::File,
    name: &str,
) -> Result<Option<fs::File>, GitError> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };

    let name = CString::new(name).map_err(|_| {
        GitError::InvalidObjectInfo("loose-object target name contains NUL".to_string())
    })?;
    // SAFETY: the directory descriptor is live, the target is
    // NUL-terminated, and a successful descriptor is immediately owned.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: `fd` is fresh from `openat` and is transferred exactly once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(GitError::InvalidObjectInfo(
            "existing loose-object target is not a regular file".to_string(),
        ));
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn validate_existing_object_file(file: fs::File, expected: &[u8]) -> Result<(), GitError> {
    validate_existing_object_reader(file, expected, "target")
}

#[cfg(test)]
mod bounded_read_tests {
    use std::io::Write;

    #[cfg(target_os = "linux")]
    use super::open_directory_tree_no_follow;
    #[cfg(target_os = "linux")]
    use super::scavenge_stale_loose_object_temps_in_dir;
    use super::{
        git_object_hash, read_git_object, read_git_object_bounded,
        reset_test_loose_object_sync_calls, test_loose_object_sync_calls,
        validate_git_object_streaming_file, write_git_object, write_git_object_with_status,
        write_git_object_with_status_inner, write_opaque_blob_with_status,
    };

    #[test]
    fn opaque_blob_entry_is_confined_to_typed_history_writer_and_helper() {
        use syn::visit::{self, Visit};

        #[derive(Default)]
        struct Calls {
            function: String,
            direct_call_operand: bool,
            calls: Vec<String>,
            violations: Vec<String>,
        }
        fn names_opaque_entry(tokens: proc_macro2::TokenStream) -> bool {
            tokens.into_iter().any(|token| match token {
                proc_macro2::TokenTree::Ident(ident) => ident == "write_opaque_blob_with_status",
                proc_macro2::TokenTree::Group(group) => names_opaque_entry(group.stream()),
                proc_macro2::TokenTree::Literal(literal) => literal
                    .to_string()
                    .contains("write_opaque_blob_with_status"),
                _ => false,
            })
        }
        impl<'ast> Visit<'ast> for Calls {
            fn visit_item(&mut self, item: &'ast syn::Item) {
                // Test modules may exercise the IO seam directly.
                if let syn::Item::Mod(module) = item
                    && module.attrs.iter().any(|attr| {
                        attr.path().is_ident("cfg")
                            && attr
                                .parse_args::<syn::Ident>()
                                .is_ok_and(|cfg| cfg == "test")
                    })
                {
                    return;
                }
                visit::visit_item(self, item);
            }

            fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
                let previous =
                    std::mem::replace(&mut self.function, function.sig.ident.to_string());
                visit::visit_item_fn(self, function);
                self.function = previous;
            }

            fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
                let previous =
                    std::mem::replace(&mut self.function, function.sig.ident.to_string());
                visit::visit_impl_item_fn(self, function);
                self.function = previous;
            }

            fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
                if rename.ident == "write_opaque_blob_with_status" {
                    self.violations
                        .push("opaque entry must not be renamed".to_string());
                }
                visit::visit_use_rename(self, rename);
            }

            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                if names_opaque_entry(mac.tokens.clone()) {
                    self.violations
                        .push("opaque entry must not be forwarded by a macro".to_string());
                }
                visit::visit_macro(self, mac);
            }

            fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
                let previous = self.direct_call_operand;
                self.direct_call_operand = matches!(&*call.func, syn::Expr::Path(_));
                self.visit_expr(&call.func);
                self.direct_call_operand = false;
                for argument in &call.args {
                    self.visit_expr(argument);
                }
                self.direct_call_operand = previous;
            }

            fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
                if path
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "write_opaque_blob_with_status")
                {
                    self.calls.push(self.function.clone());
                    if !self.direct_call_operand {
                        self.violations.push(
                            "opaque entry must not escape through a pointer or alias".to_string(),
                        );
                    }
                }
                visit::visit_expr_path(self, path);
            }
        }
        for bypass in [
            "use crate::utils::object::write_opaque_blob_with_status as fast; fn rogue(){ fast(); }",
            "fn rogue(){ let fast = crate::utils::object::write_opaque_blob_with_status; fast(); }",
            "fn rogue(){ invoke!(crate::utils::object::write_opaque_blob_with_status); }",
        ] {
            let mut visitor = Calls::default();
            visitor.visit_file(&syn::parse_file(bypass).unwrap());
            assert!(
                !visitor.violations.is_empty(),
                "guard accepted a forwarding bypass: {bypass}"
            );
        }

        fn scan(directory: &std::path::Path, calls: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, calls);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    let ast = syn::parse_file(&source).unwrap();
                    let mut visitor = Calls::default();
                    visitor.visit_file(&ast);
                    assert!(
                        visitor.violations.is_empty(),
                        "{}: {:?}",
                        path.display(),
                        visitor.violations
                    );
                    for function in visitor.calls {
                        calls.push((path.to_string_lossy().into_owned(), function));
                    }
                }
            }
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut calls = Vec::new();
        scan(&root.join("src"), &mut calls);
        calls.sort();
        let history = root
            .join("src/internal/ai/history.rs")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            calls,
            vec![
                (
                    history.clone(),
                    "run_checkpoint_object_io_helper".to_string()
                ),
                (
                    history,
                    "write_indexed_object_payload_for_attempt".to_string()
                ),
            ]
        );
    }

    #[test]
    fn opaque_fast_blob_preserves_oid_bytes_and_existing_compression() {
        for kind in [
            git_internal::hash::HashKind::Sha1,
            git_internal::hash::HashKind::Sha256,
            git_internal::hash::HashKind::Blake3,
        ] {
            let _kind = git_internal::hash::set_hash_kind_for_test(kind);
            let ordinary = tempfile::tempdir().unwrap();
            let opaque = tempfile::tempdir().unwrap();
            let payload = vec![b'x'; 128 * 1024];
            let (normal_oid, normal_created) =
                write_git_object_with_status(ordinary.path(), "blob", &payload).unwrap();
            let (fast_oid, fast_created) = write_opaque_blob_with_status(
                opaque.path(),
                &payload,
                git_internal::hash::get_hash_kind(),
            )
            .unwrap();
            assert!(normal_created && fast_created);
            assert_eq!(normal_oid, fast_oid);
            let mut canonical = format!("blob {}\0", payload.len()).into_bytes();
            canonical.extend_from_slice(&payload);
            assert_eq!(
                fast_oid,
                git_internal::hash::ObjectHash::new_for_kind(kind, &canonical)
            );
            assert_eq!(read_git_object(opaque.path(), &fast_oid).unwrap(), payload);
            let oid = fast_oid.to_string();
            let relative = std::path::Path::new("objects")
                .join(&oid[..2])
                .join(&oid[2..]);
            let normal_wire = std::fs::read(ordinary.path().join(&relative)).unwrap();
            let fast_wire = std::fs::read(opaque.path().join(&relative)).unwrap();
            // Zlib's FLEVEL header distinguishes the policies without relying on
            // the byte stream produced by a particular compressor implementation.
            assert_eq!(normal_wire[1] >> 6, 2);
            assert_eq!(fast_wire[1] >> 6, 0);
            // DEFLATE BTYPE=00 is a stored block; even compressible ciphertext
            // fixtures must not silently regain a compression CPU cost.
            assert_eq!((fast_wire[2] >> 1) & 3, 0);
            assert!(fast_wire.len() > payload.len());
            assert_eq!(
                write_opaque_blob_with_status(
                    ordinary.path(),
                    &payload,
                    git_internal::hash::get_hash_kind()
                )
                .unwrap(),
                (normal_oid, false)
            );
            assert_eq!(
                write_git_object_with_status(opaque.path(), "blob", &payload).unwrap(),
                (fast_oid, false)
            );
            assert_eq!(
                std::fs::read(ordinary.path().join(&relative)).unwrap(),
                normal_wire
            );
            assert_eq!(
                std::fs::read(opaque.path().join(&relative)).unwrap(),
                fast_wire
            );
        }
    }

    #[test]
    fn opaque_fast_blob_rejects_corrupt_existing_object_without_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"synthetic opaque archive";
        let oid = git_object_hash("blob", payload).to_string();
        let path = dir.path().join("objects").join(&oid[..2]).join(&oid[2..]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = b"not a complete zlib object";
        std::fs::write(&path, corrupt).unwrap();
        assert!(
            write_opaque_blob_with_status(dir.path(), payload, git_internal::hash::get_hash_kind())
                .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    /// Bounded reads never return more than the cap, flag truncation only
    /// when real content exceeds the cap, and truncation detection does not
    /// depend on the object header length.
    #[test]
    fn bounded_read_truncates_and_flags_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();

        // A "blob" (short header) with 1000 content bytes.
        let content = vec![b'x'; 1000];
        let hash = write_git_object(git_dir, "blob", &content).unwrap();

        // Cap above content: full read, not truncated.
        let (got, truncated) = read_git_object_bounded(git_dir, &hash, 2000).unwrap();
        assert!(!truncated);
        assert_eq!(got, content);

        // Cap exactly at content length: full read, not truncated
        // (truncation requires observing MORE than the cap).
        let (got, truncated) = read_git_object_bounded(git_dir, &hash, 1000).unwrap();
        assert!(!truncated);
        assert_eq!(got.len(), 1000);

        // Cap below content: truncated to the cap.
        let (got, truncated) = read_git_object_bounded(git_dir, &hash, 100).unwrap();
        assert!(truncated, "content beyond the cap must flag truncation");
        assert_eq!(got.len(), 100);

        // A longer object type name ("commit") does not shift truncation.
        let hash2 = write_git_object(git_dir, "commit", &content).unwrap();
        let (got, truncated) = read_git_object_bounded(git_dir, &hash2, 100).unwrap();
        assert!(truncated);
        assert_eq!(got.len(), 100);
    }

    #[test]
    fn streaming_validation_proves_held_object_size_without_materializing_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git_dir = dir.path();
        std::fs::create_dir_all(git_dir.join("objects")).expect("objects directory");
        let content = vec![b'x'; 96 * 1024];
        let oid = write_git_object(git_dir, "blob", &content).expect("write loose object");
        let oid_text = oid.to_string();
        let file = std::fs::File::open(
            git_dir
                .join("objects")
                .join(&oid_text[..2])
                .join(&oid_text[2..]),
        )
        .expect("open loose object");

        assert_eq!(
            validate_git_object_streaming_file(file, &oid, content.len() as u64)
                .expect("stream-validate held object"),
            ("blob".to_string(), content.len() as u64)
        );
    }

    /// Zero-cap reads flag truncation for any non-empty object and never
    /// allocate content.
    #[test]
    fn bounded_read_zero_cap() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        let hash = write_git_object(git_dir, "blob", b"non-empty").unwrap();
        let (got, truncated) = read_git_object_bounded(git_dir, &hash, 0).unwrap();
        assert!(truncated);
        assert!(got.is_empty());
    }

    /// A crash-created partial/corrupt file at the final object path is never
    /// trusted merely because the content-addressed name already exists.
    #[test]
    fn writer_rejects_partial_existing_object() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        let content = b"durable payload";
        let hash = git_object_hash("blob", content);
        let hash = hash.to_string();
        let object_path = git_dir.join("objects").join(&hash[..2]).join(&hash[2..]);
        std::fs::create_dir_all(object_path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&object_path).unwrap();
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        encoder.write_all(b"blob 15\0partial").unwrap();
        encoder.finish().unwrap();

        let error = write_git_object_with_status(git_dir, "blob", content).unwrap_err();
        assert!(
            error.to_string().contains("corrupt or does not match"),
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn writer_accepts_the_fixed_tmp_system_alias_without_following_repo_paths() {
        // macOS implements /tmp as the OS-owned /private/tmp symlink. A
        // descriptor-relative walk must normalize that one fixed alias while
        // continuing to reject symlinks below the repository root.
        let dir = tempfile::Builder::new()
            .prefix("libra-object-tmp-alias-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let alias = std::path::Path::new("/tmp").join(dir.path().file_name().unwrap());
        assert_eq!(
            std::fs::canonicalize(&alias).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );

        let (hash, created) = write_git_object_with_status(&alias, "blob", b"tmp alias").unwrap();
        assert!(created);
        assert!(
            alias
                .join("objects")
                .join(&hash.to_string()[..2])
                .join(&hash.to_string()[2..])
                .is_file()
        );
    }

    #[test]
    fn writers_reject_existing_object_with_trailing_storage_bytes() {
        fn plant_corrupt_object(git_dir: &std::path::Path, content: &[u8]) {
            let hash = git_object_hash("blob", content).to_string();
            let path = git_dir.join("objects").join(&hash[..2]).join(&hash[2..]);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
            encoder
                .write_all(format!("blob {}\0", content.len()).as_bytes())
                .unwrap();
            encoder.write_all(content).unwrap();
            let mut file = encoder.finish().unwrap();
            file.write_all(b"trailing-storage-garbage").unwrap();
        }

        let generic = tempfile::tempdir().unwrap();
        plant_corrupt_object(generic.path(), b"generic");
        write_git_object(generic.path(), "blob", b"generic")
            .expect_err("generic writer must reject trailing storage bytes");

        let status = tempfile::tempdir().unwrap();
        plant_corrupt_object(status.path(), b"status");
        write_git_object_with_status(status.path(), "blob", b"status")
            .expect_err("status writer must reject trailing storage bytes");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writer_scavenger_is_bounded_and_cross_oid() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("objects/ab");
        std::fs::create_dir_all(&parent).unwrap();
        let first_oid = "a".repeat(40);
        let other_oid = "b".repeat(40);
        let matching = parent.join(format!(
            ".{first_oid}.tmp-1-00000000-0000-4000-8000-000000000001"
        ));
        let other_matching = parent.join(format!(
            ".{other_oid}.tmp-2-00000000-0000-4000-8000-000000000002"
        ));
        let misleading = parent.join(format!(".{first_oid}.tmp-1-not-a-uuid"));
        let unrelated = parent.join("active-subdirectory");
        std::fs::write(&matching, b"old").unwrap();
        std::fs::write(&other_matching, b"other old").unwrap();
        std::fs::write(&misleading, b"must survive").unwrap();
        std::fs::create_dir(&unrelated).unwrap();

        let directory = open_directory_tree_no_follow(&parent).unwrap();
        scavenge_stale_loose_object_temps_in_dir(&directory, std::time::Duration::ZERO, false)
            .unwrap();

        assert!(!matching.exists());
        assert!(!other_matching.exists());
        assert!(misleading.exists());
        assert!(unrelated.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writer_scavenger_removes_sha256_temp_names() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("objects/info/libra-tmp");
        std::fs::create_dir_all(&parent).unwrap();
        let wide_oid = "d".repeat(64);
        let matching = parent.join(format!(
            ".{wide_oid}.tmp-7-00000000-0000-4000-8000-000000000007"
        ));
        std::fs::write(&matching, b"old sha256 temp").unwrap();

        let directory = open_directory_tree_no_follow(&parent).unwrap();
        scavenge_stale_loose_object_temps_in_dir(&directory, std::time::Duration::ZERO, false)
            .unwrap();

        assert!(
            !matching.exists(),
            "SHA-256 crash temp must not evade bounded scavenging"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writer_scavenger_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("objects/info/libra-tmp");
        std::fs::create_dir_all(&parent).unwrap();
        for index in 0..65 {
            std::fs::write(
                parent.join(format!(
                    ".{}.tmp-1-00000000-0000-4000-8000-{index:012}",
                    "c".repeat(40)
                )),
                b"old",
            )
            .unwrap();
        }

        let directory = open_directory_tree_no_follow(&parent).unwrap();
        scavenge_stale_loose_object_temps_in_dir(&directory, std::time::Duration::ZERO, false)
            .unwrap();
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);

        scavenge_stale_loose_object_temps_in_dir(&directory, std::time::Duration::ZERO, false)
            .unwrap();
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_symlinked_shared_temp_directory_without_deleting_outside_files() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(git_dir.join("objects/info")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("unrelated-old-file");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, git_dir.join("objects/info/libra-tmp")).unwrap();

        let error = write_git_object_with_status_inner(&git_dir, "blob", b"payload", false)
            .expect_err("symlinked temp directory must fail closed");
        assert!(!error.to_string().is_empty());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_symlinked_intermediate_temp_directory_without_deleting_victim() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("repo");
        let outside = dir.path().join("outside-info");
        let outside_temp = outside.join("libra-tmp");
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        std::fs::create_dir_all(&outside_temp).unwrap();
        let victim = outside_temp.join(format!(
            ".{}.tmp-42-00000000-0000-4000-8000-000000000042",
            "a".repeat(40)
        ));
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, git_dir.join("objects/info")).unwrap();

        write_git_object_with_status_inner(&git_dir, "blob", b"payload", false)
            .expect_err("symlinked intermediate temp directory must fail closed");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn status_writer_rejects_symlinked_objects_directory_without_external_publication() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("repo");
        let outside = dir.path().join("outside-objects");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("keep");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, git_dir.join("objects")).unwrap();

        write_git_object_with_status_inner(&git_dir, "blob", b"payload", false)
            .expect_err("symlinked objects directory must fail closed");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn status_writer_rejects_symlinked_hash_shard_without_external_publication() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("repo");
        let outside = dir.path().join("outside-shard");
        let hash = git_object_hash("blob", b"payload").to_string();
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("keep");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, git_dir.join("objects").join(&hash[..2])).unwrap();

        write_git_object_with_status_inner(&git_dir, "blob", b"payload", false)
            .expect_err("symlinked object shard must fail closed");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(!outside.join(&hash[2..]).exists());
    }

    #[cfg(unix)]
    #[test]
    fn generic_writer_preserves_legacy_umask_permissions() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        let reference = git_dir.join("reference-mode");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(&reference)
            .unwrap();
        let expected = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;

        let oid = write_git_object(git_dir, "blob", b"shared payload").unwrap();
        let oid = oid.to_string();
        let object = git_dir.join("objects").join(&oid[..2]).join(&oid[2..]);
        let actual = std::fs::metadata(object).unwrap().permissions().mode() & 0o777;
        assert_eq!(actual, expected, "loose objects must preserve 0666 & umask");
    }

    #[test]
    fn writer_fsync_follows_bulk_object_sync_data_contract() {
        reset_test_loose_object_sync_calls();
        let unsynced = tempfile::tempdir().unwrap();
        write_git_object_with_status_inner(unsynced.path(), "blob", b"unsynced", false).unwrap();
        assert_eq!(
            test_loose_object_sync_calls(),
            0,
            "default bulk object writes must not issue synchronous flushes"
        );

        reset_test_loose_object_sync_calls();
        let synced = tempfile::tempdir().unwrap();
        write_git_object_with_status_inner(synced.path(), "blob", b"synced", true).unwrap();
        assert!(
            test_loose_object_sync_calls() >= 1,
            "--sync-data mode must flush the completed loose object"
        );
    }
}
