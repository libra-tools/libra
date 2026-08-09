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
use git_internal::{errors::GitError, hash::ObjectHash};

use crate::utils::atomic_write::{self, ensure_dir_exists};

const STALE_LOOSE_OBJECT_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(target_os = "linux")]
const MAX_STALE_LOOSE_OBJECT_TEMPS_PER_WRITE: usize = 64;

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
fn open_directory_tree_no_follow(path: &Path) -> std::io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
        path::Component,
    };

    // macOS exposes the system temporary directory through `/var`, which is
    // a symlink to `/private/var`. Keep the no-follow walk strict for every
    // repository-controlled component, but normalize this fixed OS alias so
    // descriptor-relative checkpoint writes work with `tempfile::TempDir`.
    #[cfg(target_os = "macos")]
    let path = path
        .strip_prefix("/var")
        .map(|suffix| Path::new("/private/var").join(suffix))
        .unwrap_or_else(|_| path.to_path_buf());

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
    let path = path
        .strip_prefix("/var")
        .map(|suffix| Path::new("/private/var").join(suffix))
        .unwrap_or_else(|_| path.to_path_buf());

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
    let actual_hash = ObjectHash::new(&buffer);
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
    const HEADER_MAX: usize = 64;
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);
    let file = fs::File::open(object_path)?;
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
    let header = format!("{} {}\0", object_type, data.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(data);
    ObjectHash::new(&content)
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
    let header = format!("{} {}\0", object_type, data.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(data);
    let hash = git_object_hash(object_type, data);
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
        validate_existing_object_file(existing, &content)?;
        return Ok((hash, false));
    }
    #[cfg(not(unix))]
    if object_path.exists() {
        validate_existing_object(&object_path, &content)?;
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
    let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
    if let Err(error) = encoder.write_all(&content) {
        return Err(error.into());
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
                &content,
            )?;
            #[cfg(not(unix))]
            validate_existing_object(&object_path, &content)?;
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
        git_object_hash, read_git_object_bounded, reset_test_loose_object_sync_calls,
        test_loose_object_sync_calls, write_git_object, write_git_object_with_status,
        write_git_object_with_status_inner,
    };

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
