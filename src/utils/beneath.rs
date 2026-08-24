//! Repo-root fd / no-follow / beneath directory traversal (plan-20260715 WIO-02).
//!
//! Status probe and worktree scans must not follow a directory that is
//! swapped for an escaping symlink between `symlink_metadata` and `read_dir`.
//! Every listing, marker probe, and lstat is derived from a repo-root
//! directory fd/handle:
//!
//! - Linux: `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`
//! - other Unix: component-wise `openat(O_NOFOLLOW | O_DIRECTORY)`
//! - Windows: `FILE_FLAG_OPEN_REPARSE_POINT` + fail closed on a reparse point
//!
//! A path that escapes the root, or any symlink component during directory
//! descent, is returned as an I/O error (mapped to `IoBlocked` by callers).
//! There is no fallback to a naked path open after a beneath failure.

use std::{
    cell::RefCell,
    fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct RootIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
}

thread_local! {
    static CACHED_ROOT: RefCell<Option<(PathBuf, RootIdentity, fs::File)>> =
        const { RefCell::new(None) };
}

/// Open the repository working tree as a directory fd/handle.
///
/// The beneath boundary is the root directory itself, so `root`'s own final
/// component is pinned no-follow: a symlinked root is rejected. Components
/// *above* the root are outside the boundary and resolve normally — they are
/// routinely symlinks on real systems (macOS ships `/var -> private/var` and
/// `/tmp -> private/tmp`, so every `TMPDIR` path crosses one), and the callers
/// hand in a path that ordinary repository discovery already resolved.
/// Requiring them to be symlink-free would fail closed on legitimate trees
/// without adding a guarantee: once the root fd is held, descent never
/// re-traverses an ancestor, and [`root_cache_still_valid`] re-pins the
/// directory identity (which `symlink_metadata` also resolves through
/// ancestors) before any cached fd is reused.
///
/// The opened descriptor is cached per thread for the current root path, but
/// only reused when the path still names the same directory identity (so a
/// rename+recreate at the same pathname cannot keep status on a stale tree).
pub fn open_root(root: &Path) -> io::Result<fs::File> {
    let abs = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let abs = lexical_normalize(&abs);
    CACHED_ROOT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((cached, identity, file)) = slot.as_ref()
            && cached == &abs
            && root_cache_still_valid(file, identity, &abs)
        {
            return dup_file(file);
        }
        let opened = open_root_uncached(&abs)?;
        let identity = file_identity(&opened)?;
        let dup = dup_file(&opened)?;
        *slot = Some((abs, identity, opened));
        Ok(dup)
    })
}

fn root_cache_still_valid(file: &fs::File, cached: &RootIdentity, path: &Path) -> bool {
    let Ok(fd_id) = file_identity(file) else {
        return false;
    };
    if &fd_id != cached {
        return false;
    }
    let Ok(path_id) = path_identity_nofollow(path) else {
        return false;
    };
    &path_id == cached
}

#[cfg(unix)]
fn file_identity(file: &fs::File) -> io::Result<RootIdentity> {
    identity_from_metadata(&file.metadata()?)
}

#[cfg(windows)]
fn file_identity(file: &fs::File) -> io::Result<RootIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
    };
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(RootIdentity {
        volume: info.dwVolumeSerialNumber,
        index,
    })
}

#[cfg(unix)]
fn path_identity_nofollow(path: &Path) -> io::Result<RootIdentity> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::other("beneath root is a symlink"));
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "status beneath root is not a directory",
        ));
    }
    identity_from_metadata(&meta)
}

#[cfg(windows)]
fn path_identity_nofollow(path: &Path) -> io::Result<RootIdentity> {
    let file = open_windows_dir_nofollow(path)?;
    file_identity(&file)
}

#[cfg(unix)]
fn identity_from_metadata(meta: &fs::Metadata) -> io::Result<RootIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(RootIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

fn open_root_uncached(abs: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let rel = abs.strip_prefix("/").unwrap_or(abs);
        if is_root_rel(rel) {
            return fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open("/");
        }
        // Resolve the root's ancestors normally (see `open_root`: they sit
        // above the beneath boundary and are symlinks on stock macOS), then
        // pin the root component itself through `openat(O_NOFOLLOW)` so a
        // symlinked root is still rejected.
        let (parent, name) = match (abs.parent(), abs.file_name()) {
            (Some(parent), Some(name)) => (parent, name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "beneath root must be an absolute path with a final component",
                ));
            }
        };
        let parent_dir = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(parent)?;
        open_beneath_openat_walk(&parent_dir, Path::new(name))
    }
    #[cfg(windows)]
    {
        // Same contract as the Unix arm: `FILE_FLAG_OPEN_REPARSE_POINT`
        // applies to the final component, so a reparse-point root fails
        // closed while junctions above the root resolve normally.
        open_windows_dir_nofollow(abs)
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// Open `rel` beneath `root` without following any symlink component.
///
/// `rel` is worktree-relative. Empty / `.` returns a duplicate of `root`.
/// Absolute `rel` is rejected. Escape or symlink-in-descent is an error.
pub fn open_beneath(root: &fs::File, rel: &Path) -> io::Result<fs::File> {
    if rel.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath path must be worktree-relative",
        ));
    }
    if is_root_rel(rel) {
        return dup_file(root);
    }
    reject_dotdot(rel)?;
    if test_force_beneath_escape(rel) {
        return Err(io::Error::other("beneath open escaped the repository root"));
    }
    open_beneath_platform(root, rel)
}

/// `lstat` of `rel` beneath `root` (final component may be a symlink leaf).
pub fn lstat_beneath(root: &fs::File, rel: &Path) -> io::Result<RawLstat> {
    if rel.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath path must be worktree-relative",
        ));
    }
    if is_root_rel(rel) {
        return RawLstat::from_metadata(&root.metadata()?);
    }
    reject_dotdot(rel)?;
    lstat_beneath_platform(root, rel)
}

/// Marker presence (`.libra` / `.git`) via no-follow lookup under `rel`.
pub fn marker_present_beneath(root: &fs::File, rel: &Path) -> io::Result<bool> {
    let dir = open_beneath(root, rel)?;
    for marker in [crate::utils::util::ROOT_DIR, crate::utils::util::GIT_DIR] {
        match fstatat_nofollow(&dir, std::ffi::OsStr::new(marker)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Read a regular file `name` under `dir_rel` without following symlinks.
///
/// Used for `.gitignore` / `.libraignore` so ignore resolution cannot follow
/// a post-listing directory→symlink swap. Missing files are `NotFound`.
pub fn read_regular_file_beneath(
    worktree_root: &Path,
    dir_rel: &Path,
    name: &std::ffi::OsStr,
) -> io::Result<Vec<u8>> {
    let root = open_root(worktree_root)?;
    let dir = if is_root_rel(dir_rel) {
        dup_file(&root)?
    } else {
        open_beneath(&root, dir_rel)?
    };
    read_regular_file_in_dir(&dir, name)
}

fn read_regular_file_in_dir(dir: &fs::File, name: &std::ffi::OsStr) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        let c_name = component_cstring(name)?;
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        if meta.file_type().is_symlink() || meta.is_dir() {
            return Err(io::Error::other(
                "beneath ignore source is not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::BufReader::new(file), &mut bytes)?;
        Ok(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Foundation::HANDLE;
        let dir_path = final_path_name(dir.as_raw_handle() as HANDLE)?;
        let file = open_windows_nofollow(&dir_path.join(name), false, false)?;
        let root_path = final_path_name(dir.as_raw_handle() as HANDLE)?;
        assert_handle_beneath(&file, &strip_nt_prefix_path(&root_path))?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(io::Error::other(
                "beneath ignore source is not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::BufReader::new(file), &mut bytes)?;
        Ok(bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (dir, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "beneath ignore read unsupported on this platform",
        ))
    }
}

/// `read_dir` via the directory fd already opened beneath the root.
pub fn read_dir_fd(dir: fs::File) -> io::Result<FdReadDir> {
    FdReadDir::new(dir)
}

/// Portable lstat snapshot (Metadata cannot be built from `libc::stat`).
#[derive(Debug, Clone)]
pub struct RawLstat {
    pub is_symlink: bool,
    pub is_dir: bool,
    pub is_file: bool,
    pub len: u64,
    pub mode: u32,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
}

impl RawLstat {
    pub fn from_metadata(meta: &fs::Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ft = meta.file_type();
            Ok(Self {
                is_symlink: ft.is_symlink(),
                is_dir: meta.is_dir(),
                is_file: meta.is_file() && !ft.is_symlink(),
                len: meta.len(),
                mode: meta.mode(),
                ctime_sec: meta.ctime(),
                ctime_nsec: meta.ctime_nsec(),
                mtime_sec: meta.mtime(),
                mtime_nsec: meta.mtime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            let ft = meta.file_type();
            let mtime = meta.modified().ok();
            let ctime = meta.created().ok().or(mtime);
            let (ctime_sec, ctime_nsec) = system_time_parts(ctime);
            let (mtime_sec, mtime_nsec) = system_time_parts(mtime);
            Ok(Self {
                is_symlink: ft.is_symlink(),
                is_dir: meta.is_dir(),
                is_file: meta.is_file() && !ft.is_symlink(),
                len: meta.len(),
                mode: 0,
                ctime_sec,
                ctime_nsec,
                mtime_sec,
                mtime_nsec,
            })
        }
    }

    #[cfg(unix)]
    fn from_libc_stat(stat: &libc::stat) -> Self {
        let mode = stat.st_mode;
        let is_symlink = (mode & libc::S_IFMT) == libc::S_IFLNK;
        let is_dir = (mode & libc::S_IFMT) == libc::S_IFDIR;
        let is_file = (mode & libc::S_IFMT) == libc::S_IFREG;
        Self {
            is_symlink,
            is_dir,
            is_file: is_file && !is_symlink,
            len: stat.st_size as u64,
            mode: {
                #[cfg(target_os = "linux")]
                {
                    mode
                }
                #[cfg(not(target_os = "linux"))]
                {
                    mode as u32
                }
            },
            ctime_sec: stat.st_ctime,
            ctime_nsec: ctime_nsec(stat),
            mtime_sec: stat.st_mtime,
            mtime_nsec: mtime_nsec(stat),
        }
    }
}

#[cfg(unix)]
fn ctime_nsec(stat: &libc::stat) -> i64 {
    // The Rust `libc` crate flattens Darwin's `st_ctimespec` into
    // `st_ctime`/`st_ctime_nsec` (same names as Linux); `st_ctimespec`
    // does not exist on `libc::stat` there — the aarch64-apple-darwin
    // release build of v0.20.0 was the first cross-target compile since
    // WIO-02 landed and failed on exactly this field (W5-09).
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    {
        stat.st_ctime_nsec
    }
    #[cfg(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        stat.st_ctim.tv_nsec
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = stat;
        0
    }
}

#[cfg(unix)]
fn mtime_nsec(stat: &libc::stat) -> i64 {
    // See `ctime_nsec`: libc flattens Darwin's timespec pair to
    // `st_mtime`/`st_mtime_nsec`.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    {
        stat.st_mtime_nsec
    }
    #[cfg(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        stat.st_mtim.tv_nsec
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = stat;
        0
    }
}

#[cfg(not(unix))]
fn system_time_parts(time: Option<std::time::SystemTime>) -> (i64, i64) {
    use std::time::UNIX_EPOCH;
    let Some(time) = time else {
        return (0, 0);
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (
            duration.as_secs() as i64,
            i64::from(duration.subsec_nanos()),
        ),
        Err(_) => (0, 0),
    }
}

/// Handle-bound Windows directory stream (`GetFileInformationByHandleEx`).
#[cfg(not(unix))]
struct WindowsDirStream {
    _file: fs::File,
    handle: windows_sys::Win32::Foundation::HANDLE,
    buffer: Vec<u8>,
    pos: usize,
    restart: bool,
    done: bool,
}

/// Streaming directory iterator over an already-opened directory fd.
pub struct FdReadDir {
    #[cfg(unix)]
    dir: *mut libc::DIR,
    #[cfg(not(unix))]
    win: WindowsDirStream,
}

// SAFETY: `DIR*` is used from a single worker thread; the iterator is not shared.
#[cfg(unix)]
unsafe impl Send for FdReadDir {}

impl FdReadDir {
    fn new(dir: fs::File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, IntoRawFd};
            // `open_root` returns `dup` of a thread-local cached directory fd.
            // On Unix, `dup` shares the open-file offset with the cache, so a
            // prior `fdopendir`/`readdir` that drained the description would
            // make the next listing look empty (WIO-02 probe regression).
            // Rewind before handing the fd to `fdopendir`.
            if unsafe { libc::lseek(dir.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = dir.into_raw_fd();
            let ptr = unsafe { libc::fdopendir(fd) };
            if ptr.is_null() {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                return Err(error);
            }
            Ok(Self { dir: ptr })
        }
        #[cfg(not(unix))]
        {
            use std::os::windows::io::AsRawHandle;

            use windows_sys::Win32::Foundation::HANDLE;
            let handle = dir.as_raw_handle() as HANDLE;
            let buffer = vec![0u8; 16 * 1024];
            let pos = buffer.len();
            Ok(Self {
                win: WindowsDirStream {
                    _file: dir,
                    handle,
                    buffer,
                    pos,
                    restart: true,
                    done: false,
                },
            })
        }
    }
}

#[cfg(unix)]
impl Drop for FdReadDir {
    fn drop(&mut self) {
        if !self.dir.is_null() {
            unsafe {
                libc::closedir(self.dir);
            }
            self.dir = std::ptr::null_mut();
        }
    }
}

/// One `readdir` record with `d_type` when the filesystem provides it.
#[derive(Debug)]
pub struct FdDirent {
    pub name: std::ffi::OsString,
    pub d_type: u8,
}

#[cfg(not(unix))]
impl WindowsDirStream {
    fn next(&mut self) -> Option<io::Result<FdDirent>> {
        loop {
            if self.done {
                return None;
            }
            if self.pos >= self.buffer.len() {
                if let Err(error) = self.refill() {
                    return Some(Err(error));
                }
                if self.done {
                    return None;
                }
            }
            match self.parse_one() {
                Ok(Some(entry)) => {
                    if entry.name == "." || entry.name == ".." {
                        continue;
                    }
                    return Some(Ok(entry));
                }
                Ok(None) => continue,
                Err(error) => return Some(Err(error)),
            }
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
        const FILE_FULL_DIRECTORY_INFO: i32 = 14;
        const FILE_FULL_DIRECTORY_RESTART_INFO: i32 = 15;
        let class = if self.restart {
            FILE_FULL_DIRECTORY_RESTART_INFO
        } else {
            FILE_FULL_DIRECTORY_INFO
        };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                self.handle,
                class,
                self.buffer.as_mut_ptr().cast(),
                self.buffer.len() as u32,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(18) {
                self.done = true;
                return Ok(());
            }
            return Err(error);
        }
        self.restart = false;
        self.pos = 0;
        Ok(())
    }

    fn parse_one(&mut self) -> io::Result<Option<FdDirent>> {
        const HEADER: usize = 68;
        if self.pos + HEADER > self.buffer.len() {
            self.pos = self.buffer.len();
            return Ok(None);
        }
        let next_offset = u32::from_le_bytes(
            self.buffer[self.pos..self.pos + 4]
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dir info offset"))?,
        ) as usize;
        let attrs = u32::from_le_bytes(
            self.buffer[self.pos + 56..self.pos + 60]
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dir info attrs"))?,
        );
        let name_len = u32::from_le_bytes(
            self.buffer[self.pos + 60..self.pos + 64]
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dir info name len"))?,
        ) as usize;
        let name_off = self.pos + HEADER;
        if name_off + name_len > self.buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated directory info name",
            ));
        }
        let mut wide = Vec::with_capacity(name_len / 2);
        let mut index = 0;
        while index + 1 < name_len {
            wide.push(u16::from_le_bytes([
                self.buffer[name_off + index],
                self.buffer[name_off + index + 1],
            ]));
            index += 2;
        }
        use std::os::windows::ffi::OsStringExt;
        let name = std::ffi::OsString::from_wide(&wide);
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let d_type = if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            10
        } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
            4
        } else {
            8
        };
        if next_offset == 0 {
            self.pos = self.buffer.len();
        } else {
            self.pos += next_offset;
        }
        Ok(Some(FdDirent { name, d_type }))
    }
}

impl Iterator for FdReadDir {
    type Item = io::Result<FdDirent>;

    fn next(&mut self) -> Option<Self::Item> {
        #[cfg(unix)]
        {
            use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
            loop {
                clear_errno();
                let entry = unsafe { libc::readdir(self.dir) };
                if entry.is_null() {
                    let err = last_errno();
                    if err == 0 {
                        return None;
                    }
                    return Some(Err(io::Error::from_raw_os_error(err)));
                }
                let c_name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
                let bytes = c_name.to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let d_type = unsafe { (*entry).d_type };
                return Some(Ok(FdDirent {
                    name: OsStr::from_bytes(bytes).to_os_string(),
                    d_type,
                }));
            }
        }
        #[cfg(not(unix))]
        {
            self.win.next()
        }
    }
}

fn is_root_rel(rel: &Path) -> bool {
    rel.as_os_str().is_empty()
        || rel == Path::new(".")
        || rel
            .components()
            .all(|component| matches!(component, Component::CurDir))
}

fn reject_dotdot(rel: &Path) -> io::Result<()> {
    for component in rel.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "beneath path must not contain '..' or a prefix",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rel_cstring(rel: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(rel.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath path contains interior NUL",
        )
    })
}

#[cfg(unix)]
fn component_cstring(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath component contains interior NUL",
        )
    })
}

#[cfg(unix)]
fn clear_errno() {
    unsafe {
        *errno_location() = 0;
    }
}

#[cfg(unix)]
fn errno_location() -> *mut libc::c_int {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "fuchsia",
        target_os = "redox",
    ))]
    {
        unsafe { libc::__errno_location() }
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
    ))]
    {
        unsafe { libc::__error() }
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    {
        unsafe { libc::___errno() }
    }
    #[cfg(all(
        unix,
        not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "emscripten",
            target_os = "hurd",
            target_os = "fuchsia",
            target_os = "redox",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly",
            target_os = "solaris",
            target_os = "illumos",
        ))
    ))]
    {
        // Remaining Unix targets (e.g. Haiku) still need a zeroed errno
        // before readdir so EOF is not misread as an I/O error.
        unsafe extern "C" {
            fn __errno() -> *mut libc::c_int;
        }
        unsafe { __errno() }
    }
}

#[cfg(unix)]
fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(unix)]
fn dup_file(file: &fs::File) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe { libc::dup(file.as_raw_fd()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn dup_file(file: &fs::File) -> io::Result<fs::File> {
    file.try_clone()
}

#[cfg(unix)]
fn open_beneath_openat_walk(root: &fs::File, rel: &Path) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut current = dup_file(root)?;
    for component in rel.components() {
        let name = match component {
            Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "beneath path must not contain '..' or a prefix",
                ));
            }
        };
        let c_name = component_cstring(name)?;
        let next = unsafe {
            libc::openat(
                current.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { fs::File::from_raw_fd(next) };
    }
    Ok(current)
}

#[cfg(target_os = "linux")]
fn open_beneath_platform(root: &fs::File, rel: &Path) -> io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let c_rel = rel_cstring(rel)?;
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let mut how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            c_rel.as_ptr(),
            &mut how as *mut OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP) | Some(libc::EPERM)
        ) {
            return open_beneath_openat_walk(root, rel);
        }
        return Err(error);
    }
    Ok(unsafe { fs::File::from_raw_fd(fd as i32) })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_beneath_platform(root: &fs::File, rel: &Path) -> io::Result<fs::File> {
    open_beneath_openat_walk(root, rel)
}

#[cfg(windows)]
fn open_windows_dir_nofollow(path: &Path) -> io::Result<fs::File> {
    open_windows_nofollow(path, false, true)
}

/// Open `path` with `FILE_FLAG_OPEN_REPARSE_POINT` (final component only).
/// Intermediate reparse points are not followed when callers walk one
/// component at a time. `allow_reparse_leaf` keeps a final symlink/junction
/// as the opened object; `require_dir` rejects non-directories.
#[cfg(windows)]
fn open_windows_nofollow(
    path: &Path,
    allow_reparse_leaf: bool,
    require_dir: bool,
) -> io::Result<fs::File> {
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle},
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetFileInformationByHandle, OPEN_EXISTING, SYNCHRONIZE,
        },
    };

    let wide = wide_path(path)?;
    let desired_access = if require_dir {
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE
    } else {
        // Leaf lstat must not demand FILE_READ_DATA (aliased with
        // FILE_LIST_DIRECTORY) — attribute-only ACLs previously worked via
        // symlink_metadata and must keep working under beneath.
        FILE_READ_ATTRIBUTES | SYNCHRONIZE
    };
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle as _) };
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let ok = unsafe { GetFileInformationByHandle(owned.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if !allow_reparse_leaf && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("beneath open hit a reparse point"));
    }
    if require_dir && info.dwFileAttributes & 0x10 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "status beneath path is not a directory",
        ));
    }
    Ok(fs::File::from(owned))
}

#[cfg(windows)]
fn open_beneath_platform(root: &fs::File, rel: &Path) -> io::Result<fs::File> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;

    let root_path = final_path_name(root.as_raw_handle() as HANDLE)?;
    let mut expected = root_path.clone();
    let mut last: Option<fs::File> = None;
    for component in rel.components() {
        let name = match component {
            Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "beneath path must not contain '..' or a prefix",
                ));
            }
        };
        expected = expected.join(name);
        let file = open_windows_dir_nofollow(&expected)?;
        assert_final_path_matches(&file, &expected)?;
        if !path_is_beneath(
            &final_path_name(file.as_raw_handle() as HANDLE)?,
            &root_path,
        ) {
            return Err(io::Error::other("beneath open escaped the repository root"));
        }
        last = Some(file);
    }
    last.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "beneath path had no component"))
}

#[cfg(windows)]
fn final_path_name(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_DOS};
    let mut buf = vec![0u16; 1024];
    let len = unsafe {
        GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), buf.len() as u32, VOLUME_NAME_DOS)
    };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if (len as usize) >= buf.len() {
        buf.resize(len as usize + 1, 0);
        let len = unsafe {
            GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), buf.len() as u32, VOLUME_NAME_DOS)
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(len as usize);
    } else {
        buf.truncate(len as usize);
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
}

#[cfg(windows)]
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath path contains interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn path_is_beneath(candidate: &Path, root: &Path) -> bool {
    let candidate = strip_nt_prefix_path(candidate);
    let root = strip_nt_prefix_path(root);
    paths_equal_windows(&candidate, &root) || path_starts_with_windows(&candidate, &root)
}

#[cfg(windows)]
fn assert_handle_beneath(child: &fs::File, root_path: &Path) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    let child_path = final_path_name(child.as_raw_handle() as HANDLE)?;
    if !path_is_beneath(&child_path, root_path) {
        return Err(io::Error::other("beneath open escaped the repository root"));
    }
    Ok(())
}

#[cfg(windows)]
fn assert_final_path_matches(file: &fs::File, expected: &Path) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    let opened = final_path_name(file.as_raw_handle() as HANDLE)?;
    if !paths_equal_windows(
        &strip_nt_prefix_path(&opened),
        &strip_nt_prefix_path(expected),
    ) {
        return Err(io::Error::other(
            "beneath open resolved to a different path than requested",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn strip_nt_prefix_path(path: &Path) -> PathBuf {
    let rendered = path.to_string_lossy();
    if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = rendered.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(windows)]
fn paths_equal_windows(left: &Path, right: &Path) -> bool {
    left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
}

#[cfg(windows)]
fn path_starts_with_windows(candidate: &Path, root: &Path) -> bool {
    let candidate = candidate.to_string_lossy();
    let root = root.to_string_lossy();
    if candidate.len() <= root.len() {
        return false;
    }
    if !candidate
        .as_bytes()
        .get(..root.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root.as_bytes()))
    {
        return false;
    }
    candidate.as_bytes().get(root.len()) == Some(&b'\\')
        || candidate.as_bytes().get(root.len()) == Some(&b'/')
}

#[cfg(unix)]
fn lstat_beneath_platform(root: &fs::File, rel: &Path) -> io::Result<RawLstat> {
    let parent = rel.parent().filter(|p| !is_root_rel(p));
    let name = rel.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath lstat missing file name",
        )
    })?;
    let dir = match parent {
        Some(parent) => open_beneath_platform(root, parent)?,
        None => dup_file(root)?,
    };
    fstatat_raw(&dir, name)
}

#[cfg(windows)]
fn lstat_beneath_platform(root: &fs::File, rel: &Path) -> io::Result<RawLstat> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    let parent = rel.parent().filter(|p| !is_root_rel(p));
    let name = rel.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneath lstat missing file name",
        )
    })?;
    let dir = match parent {
        Some(parent) => open_beneath_platform(root, parent)?,
        None => dup_file(root)?,
    };
    let root_path = final_path_name(root.as_raw_handle() as HANDLE)?;
    let dir_path = final_path_name(dir.as_raw_handle() as HANDLE)?;
    let file = open_windows_nofollow(&dir_path.join(name), true, false)?;
    assert_handle_beneath(&file, &root_path)?;
    drop(dir);
    RawLstat::from_metadata(&file.metadata()?)
}

fn fstatat_nofollow(dir: &fs::File, name: &std::ffi::OsStr) -> io::Result<RawLstat> {
    #[cfg(unix)]
    {
        fstatat_raw(dir, name)
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Foundation::HANDLE;
        let dir_path = final_path_name(dir.as_raw_handle() as HANDLE)?;
        let file = open_windows_nofollow(&dir_path.join(name), true, false)?;
        assert_handle_beneath(&file, &dir_path)?;
        RawLstat::from_metadata(&file.metadata()?)
    }
}

#[cfg(unix)]
fn fstatat_raw(dir: &fs::File, name: &std::ffi::OsStr) -> io::Result<RawLstat> {
    use std::os::fd::AsRawFd;
    let c_name = component_cstring(name)?;
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RawLstat::from_libc_stat(&stat))
}

/// Test harness seam: when `LIBRA_TEST=1` and the swap env vars are set,
/// force the matching relative open to fail closed with the same error the
/// real beneath path returns on escape. This is intentionally **non-mutating**
/// (no `remove_dir_all` / symlink injection in the shipped binary); real
/// check→open races are covered by unit tests that mutate only their tempdirs.
fn test_force_beneath_escape(rel: &Path) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return false;
    }
    let Ok(target) = std::env::var("LIBRA_TEST_BENEATH_TOCTOU_SWAP") else {
        return false;
    };
    if rel != Path::new(&target) && !rel.ends_with(&target) {
        return false;
    }
    // OUTSIDE must be set so callers cannot arm the seam by accident with a
    // single env var; the path itself is unused because the seam is
    // non-mutating.
    std::env::var_os("LIBRA_TEST_BENEATH_TOCTOU_OUTSIDE").is_some()
}

#[cfg(all(test, windows))]
fn create_windows_test_reparse(link: &Path, target: &Path) -> io::Result<()> {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()?;
    if status.success() {
        return Ok(());
    }
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn open_root_rejects_symlink() {
        let real = tempfile::tempdir().expect("real");
        let link_dir = tempfile::tempdir().expect("link parent");
        let link = link_dir.path().join("rootlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let error = open_root(&link).expect_err("symlinked root");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn open_root_invalidates_stale_cache_after_root_replace() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root_path = tmp.path().join("wt");
        fs::create_dir(&root_path).unwrap();
        fs::write(root_path.join("old.txt"), b"old\n").unwrap();
        let first = open_root(&root_path).expect("first open");
        let first_names: Vec<_> = read_dir_fd(first)
            .expect("list first")
            .map(|entry| entry.expect("dirent").name)
            .collect();
        assert!(first_names.iter().any(|n| n == "old.txt"));

        let retired = tmp.path().join("retired");
        fs::rename(&root_path, &retired).unwrap();
        fs::create_dir(&root_path).unwrap();
        fs::write(root_path.join("new.txt"), b"new\n").unwrap();

        let second = open_root(&root_path).expect("reopen after replace");
        let second_names: Vec<_> = read_dir_fd(second)
            .expect("list second")
            .map(|entry| entry.expect("dirent").name)
            .collect();
        assert!(second_names.iter().any(|n| n == "new.txt"));
        assert!(!second_names.iter().any(|n| n == "old.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn open_beneath_rejects_escaping_symlink() {
        let root_dir = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("decoy.txt"), b"secret\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), root_dir.path().join("link")).unwrap();
        let root = open_root(root_dir.path()).expect("open root");
        let error = open_beneath(&root, Path::new("link")).expect_err("symlink must fail");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn open_beneath_accepts_real_subdirectory() {
        let root_dir = tempfile::tempdir().expect("root");
        fs::create_dir(root_dir.path().join("inner")).unwrap();
        fs::write(root_dir.path().join("inner/a.txt"), b"ok\n").unwrap();
        let root = open_root(root_dir.path()).expect("open root");
        let dir = open_beneath(&root, Path::new("inner")).expect("real dir");
        let listing = read_dir_fd(dir).expect("fdopendir");
        let names: Vec<_> = listing.map(|entry| entry.expect("dirent").name).collect();
        assert!(names.iter().any(|n| n == "a.txt"));
    }

    /// Cached `open_root` returns `dup` of one open-file description. A first
    /// `read_dir_fd` that drains the directory must not make the next listing
    /// through the same cache appear empty.
    #[cfg(unix)]
    #[test]
    fn read_dir_fd_rewinds_shared_cached_root_offset() {
        let root_dir = tempfile::tempdir().expect("root");
        fs::write(root_dir.path().join("visible.txt"), b"ok\n").unwrap();
        let first = open_beneath(&open_root(root_dir.path()).expect("root"), Path::new(""))
            .and_then(read_dir_fd)
            .expect("first listing");
        let first_names: Vec<_> = first.map(|entry| entry.expect("dirent").name).collect();
        assert!(
            first_names.iter().any(|n| n == "visible.txt"),
            "first listing sees the file: {first_names:?}"
        );
        let second = open_beneath(&open_root(root_dir.path()).expect("root"), Path::new(""))
            .and_then(read_dir_fd)
            .expect("second listing");
        let second_names: Vec<_> = second.map(|entry| entry.expect("dirent").name).collect();
        assert!(
            second_names.iter().any(|n| n == "visible.txt"),
            "second listing must rewind the shared cached offset: {second_names:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn open_beneath_rejects_directory_swapped_for_escape() {
        let root_dir = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("decoy.txt"), b"secret\n").unwrap();
        fs::create_dir(root_dir.path().join("swapme")).unwrap();
        fs::write(root_dir.path().join("swapme/inside.txt"), b"ok\n").unwrap();
        let root = open_root(root_dir.path()).expect("open root");
        open_beneath(&root, Path::new("swapme")).expect("pre-swap dir");
        fs::remove_dir_all(root_dir.path().join("swapme")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root_dir.path().join("swapme")).unwrap();
        let error = open_beneath(&root, Path::new("swapme")).expect_err("swapped symlink");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn lstat_beneath_sees_symlink_leaf_without_following() {
        let root_dir = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("decoy.txt"), b"secret\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("decoy.txt"),
            root_dir.path().join("leaf"),
        )
        .unwrap();
        let root = open_root(root_dir.path()).expect("open root");
        let stat = lstat_beneath(&root, Path::new("leaf")).expect("lstat leaf");
        assert!(stat.is_symlink);
        assert!(!stat.is_dir);
        assert!(!stat.is_file);
    }

    #[cfg(windows)]
    #[test]
    fn open_beneath_rejects_directory_junction() {
        let root_dir = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("decoy.txt"), b"secret\n").unwrap();
        let link = root_dir.path().join("link");
        create_windows_test_reparse(&link, outside.path()).expect("junction");
        let root = open_root(root_dir.path()).expect("open root");
        let error = open_beneath(&root, Path::new("link")).expect_err("junction must fail");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(windows)]
    #[test]
    fn open_beneath_rejects_directory_swapped_for_junction() {
        let root_dir = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("decoy.txt"), b"secret\n").unwrap();
        fs::create_dir(root_dir.path().join("swapme")).unwrap();
        fs::write(root_dir.path().join("swapme/inside.txt"), b"ok\n").unwrap();
        let root = open_root(root_dir.path()).expect("open root");
        open_beneath(&root, Path::new("swapme")).expect("pre-swap dir");
        fs::remove_dir_all(root_dir.path().join("swapme")).unwrap();
        create_windows_test_reparse(&root_dir.path().join("swapme"), outside.path())
            .expect("junction");
        let error = open_beneath(&root, Path::new("swapme")).expect_err("swapped junction");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }
}
