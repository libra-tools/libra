//! Out-of-process recyclable status I/O worker (plan-20260715 WIO-01).
//!
//! Basic scan / probe syscalls run in a bounded pool of helper processes
//! (cap 8). The parent keeps a stably sorted pending queue; a stuck task is
//! killed by process group and its slot is reused. Streaming `read_dir`
//! emits `Begin` / `Record` / `Checkpoint` so a mid-stream kill keeps the
//! last checkpointed partial and marks the current edge `IoBlocked`.
//!
//! The helper entry (`--libra-internal-status-io-worker`) is handled in
//! `main` before upgrade, recovery, or any repository write. It accepts only
//! an anonymous pipe plus a capability token.

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::{OsStr, OsString},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

/// Hidden argv token. Must be the second argv element; parsed in `main` before CLI.
pub const STATUS_IO_WORKER_ARG: &str = "--libra-internal-status-io-worker";
/// Capability token env. Worker exits 2 if missing or mismatched.
pub const STATUS_IO_WORKER_CAP_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_CAP";
/// Parent pid, so a helper blocked in a syscall can still exit when status dies.
pub const STATUS_IO_WORKER_PPID_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_PPID";

const MAX_INFLIGHT: usize = 8;
/// Cap on queued work waiting for a dispatcher slot. Callers still wait
/// (not fail-fast at 8), but timed-out / excess jobs must not grow without
/// bound in a long-lived host.
const MAX_PENDING: usize = 64;
const FRAME_CAP: usize = 8 * 1024 * 1024;

/// Serializable worktree stat (Metadata cannot cross the process boundary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedStat {
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

impl CapturedStat {
    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ft = meta.file_type();
            Self {
                is_symlink: ft.is_symlink(),
                is_dir: meta.is_dir(),
                is_file: meta.is_file() && !ft.is_symlink(),
                len: meta.len(),
                mode: meta.mode(),
                ctime_sec: meta.ctime(),
                ctime_nsec: meta.ctime_nsec(),
                mtime_sec: meta.mtime(),
                mtime_nsec: meta.mtime_nsec(),
            }
        }
        #[cfg(not(unix))]
        {
            let ft = meta.file_type();
            let mtime = meta.modified().ok();
            let ctime = meta.created().ok().or(mtime);
            let (ctime_sec, ctime_nsec) = system_time_parts(ctime);
            let (mtime_sec, mtime_nsec) = system_time_parts(mtime);
            Self {
                is_symlink: ft.is_symlink(),
                is_dir: meta.is_dir(),
                is_file: meta.is_file() && !ft.is_symlink(),
                len: meta.len(),
                mode: 0,
                ctime_sec,
                ctime_nsec,
                mtime_sec,
                mtime_nsec,
            }
        }
    }

    pub(crate) fn is_symlink(&self) -> bool {
        self.is_symlink
    }

    pub(crate) fn is_dir(&self) -> bool {
        self.is_dir
    }

    pub(crate) fn is_file(&self) -> bool {
        self.is_file
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    fn from_raw_lstat(raw: &crate::utils::beneath::RawLstat) -> Self {
        Self {
            is_symlink: raw.is_symlink,
            is_dir: raw.is_dir,
            is_file: raw.is_file,
            len: raw.len,
            mode: raw.mode,
            ctime_sec: raw.ctime_sec,
            ctime_nsec: raw.ctime_nsec,
            mtime_sec: raw.mtime_sec,
            mtime_nsec: raw.mtime_nsec,
        }
    }
}

#[cfg(not(unix))]
fn system_time_parts(time: Option<std::time::SystemTime>) -> (i64, i64) {
    use std::time::{SystemTime, UNIX_EPOCH};
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

/// One `read_dir` name plus the worker-side `file_type()` (d_type / lstat).
/// Callers must not issue a follow-up IPC stat for the common typed case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Dirent {
    pub name: Vec<u8>,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_file: bool,
    /// `false` when `DirEntry::file_type()` failed; caller may fall back to
    /// a killable `deadline_stat` for this one name.
    pub type_ok: bool,
}

impl Dirent {
    fn from_dir_entry(entry: &std::fs::DirEntry) -> Self {
        let name = path_to_bytes(&PathBuf::from(entry.file_name()));
        match entry.file_type() {
            Ok(file_type) => Self {
                name,
                is_dir: file_type.is_dir(),
                is_symlink: file_type.is_symlink(),
                is_file: file_type.is_file() && !file_type.is_symlink(),
                type_ok: true,
            },
            Err(_) => Self {
                name,
                is_dir: false,
                is_symlink: false,
                is_file: false,
                type_ok: false,
            },
        }
    }

    fn from_fd_dirent(entry: &crate::utils::beneath::FdDirent) -> Self {
        let name = path_to_bytes(&PathBuf::from(&entry.name));
        // `d_type` values match libc DT_* / Windows FILE_ATTRIBUTE mapping.
        const DT_UNKNOWN: u8 = 0;
        const DT_DIR: u8 = 4;
        const DT_REG: u8 = 8;
        const DT_LNK: u8 = 10;
        match entry.d_type {
            DT_DIR => Self {
                name,
                is_dir: true,
                is_symlink: false,
                is_file: false,
                type_ok: true,
            },
            DT_LNK => Self {
                name,
                is_dir: false,
                is_symlink: true,
                is_file: false,
                type_ok: true,
            },
            DT_REG => Self {
                name,
                is_dir: false,
                is_symlink: false,
                is_file: true,
                type_ok: true,
            },
            DT_UNKNOWN => Self {
                name,
                is_dir: false,
                is_symlink: false,
                is_file: false,
                type_ok: false,
            },
            _ => Self {
                name,
                is_dir: false,
                is_symlink: false,
                is_file: false,
                type_ok: true,
            },
        }
    }
}

/// Cheap classify result from a `Dirent` or a fallback `CapturedStat`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirentKind {
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
}

impl DirentKind {
    pub(crate) fn is_dir(self) -> bool {
        self.is_dir
    }

    pub(crate) fn is_file(self) -> bool {
        self.is_file
    }

    pub(crate) fn is_symlink(self) -> bool {
        self.is_symlink
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReadDirListing {
    pub entries: Vec<Dirent>,
    pub error_kinds: Vec<(u8, Option<i32>)>,
    pub taken: usize,
    pub hit_cap: bool,
    /// Set by the parent when the worker was killed mid-stream; not sent
    /// on the wire (`#[serde(default)]`).
    #[serde(default)]
    pub timed_out: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapRequest {
    cap: String,
    request: IoRequest,
}

#[derive(Debug, Serialize, Deserialize)]
enum IoRequest {
    SymlinkMetadata {
        path: Vec<u8>,
        /// Worktree root. Empty → legacy path lstat (library callers).
        root: Vec<u8>,
    },
    CanonicalizePair {
        left: Vec<u8>,
        right: Vec<u8>,
    },
    ReadDir {
        path: Vec<u8>,
        /// Worktree root. Empty → legacy path `read_dir` (library callers).
        root: Vec<u8>,
        remaining: usize,
        checkpoint_every: u32,
    },
    FileBlobHash {
        path: Vec<u8>,
        hash_kind: String,
        /// Worktree root for LFS/attributes discovery. Recycled helpers keep
        /// the spawn CWD; the parent always sends the request repo here.
        workdir: Vec<u8>,
    },
    /// Local object-store blob read (WIO-03). Parent peels replace refs;
    /// worker opens `objects_root` with a local-only backend and never
    /// hydrates or writes.
    ReadObjectBlob {
        oid: String,
        objects_root: Vec<u8>,
        byte_limit: u64,
        hash_kind: String,
    },
    MarkerProbe {
        dir: Vec<u8>,
        /// Worktree root. Empty → legacy path marker probe (library callers).
        root: Vec<u8>,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
enum IoEvent {
    Ready,
    Begin,
    RecordDirent(Dirent),
    RecordError {
        kind: u8,
        raw_os: Option<i32>,
    },
    Checkpoint {
        seq: u64,
        records: u64,
    },
    DoneStat {
        result: WireResult<CapturedStat>,
    },
    DoneCanonicalize {
        left: WireResult<Vec<u8>>,
        right: WireResult<Vec<u8>>,
    },
    DoneReadDir {
        listing: ReadDirListing,
    },
    DoneHash {
        hex: WireResult<String>,
    },
    DoneObjectBlob {
        status: ObjectBlobStatus,
        /// Filled by the parent after a trailing binary frame when
        /// `status == Ok`. Never JSON-encoded (WIO-03 ≤20% overhead).
        #[serde(skip)]
        bytes: Option<Vec<u8>>,
    },
    DoneMarker {
        present: Option<bool>,
        err_kind: Option<u8>,
        err_raw_os: Option<i32>,
    },
    Error {
        message: String,
    },
}

/// Compact object-read status on the wire (bytes travel in a raw frame).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ObjectBlobStatus {
    Ok,
    Missing,
    Corrupt,
    Unavailable,
    TooLarge,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
enum WireResult<T> {
    Ok(T),
    Err { kind: u8, raw_os: Option<i32> },
}

fn kind_to_u8(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::NotFound => 0,
        io::ErrorKind::PermissionDenied => 1,
        io::ErrorKind::TimedOut => 2,
        _ => 3,
    }
}

pub(crate) fn io_from_wire(kind: u8, raw_os: Option<i32>) -> io::Error {
    if let Some(code) = raw_os {
        return io::Error::from_raw_os_error(code);
    }
    let kind = match kind {
        0 => io::ErrorKind::NotFound,
        1 => io::ErrorKind::PermissionDenied,
        2 => io::ErrorKind::TimedOut,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, "status io worker")
}

fn wire_result<T>(result: io::Result<T>) -> WireResult<T> {
    match result {
        Ok(value) => WireResult::Ok(value),
        Err(error) => WireResult::Err {
            kind: kind_to_u8(error.kind()),
            raw_os: error.raw_os_error(),
        },
    }
}

fn unwrap_wire<T>(result: WireResult<T>) -> io::Result<T> {
    match result {
        WireResult::Ok(value) => Ok(value),
        WireResult::Err { kind, raw_os } => Err(io_from_wire(kind, raw_os)),
    }
}

fn path_to_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(|unit| unit.to_le_bytes())
            .collect()
    }
}

fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(OsStr::from_bytes(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut wide = Vec::with_capacity(bytes.len() / 2);
        let mut chunks = bytes.chunks_exact(2);
        for chunk in &mut chunks {
            wide.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        PathBuf::from(OsString::from_wide(&wide))
    }
}

pub(crate) fn dirent_os(bytes: &[u8]) -> OsString {
    bytes_to_path(bytes).into_os_string()
}

fn write_frame(writer: &mut impl Write, event: &IoEvent) -> io::Result<()> {
    let payload = serde_json::to_vec(event)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > FRAME_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker frame too large",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > FRAME_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker frame length invalid",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn parent_still_alive(ppid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::getppid() as u32 == ppid }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, ppid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = ppid;
        true
    }
}

fn start_parent_watchdog() {
    let Ok(ppid) = std::env::var(STATUS_IO_WORKER_PPID_ENV) else {
        return;
    };
    let Ok(ppid) = ppid.parse::<u32>() else {
        return;
    };
    if ppid == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("libra-status-io-ppid".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if !parent_still_alive(ppid) {
                    std::process::exit(1);
                }
            }
        });
}

/// Worker main: capability check, then serve framed requests until EOF.
pub fn run_worker() -> i32 {
    let expected = match std::env::var(STATUS_IO_WORKER_CAP_ENV) {
        Ok(value) if !value.is_empty() => value,
        _ => return 2,
    };
    start_parent_watchdog();
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    if write_frame(&mut stdout, &IoEvent::Ready).is_err() {
        return 1;
    }
    loop {
        let wrapped: CapRequest = match read_frame(&mut stdin) {
            Ok(wrapped) => wrapped,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return 0,
            Err(_) => return 1,
        };
        if wrapped.cap != expected {
            return 2;
        }
        match handle_request(wrapped.request, &mut stdout) {
            Ok(true) => {}
            Ok(false) => return 0,
            Err(_) => return 1,
        }
    }
}

fn handle_request(request: IoRequest, stdout: &mut impl Write) -> io::Result<bool> {
    match request {
        IoRequest::Shutdown => return Ok(false),
        IoRequest::SymlinkMetadata { path, root } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let root_path = bytes_to_path(&root);
            let result = lstat_request(&path, &root_path);
            write_frame(stdout, &IoEvent::DoneStat { result })?;
        }
        IoRequest::CanonicalizePair { left, right } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let left_path = bytes_to_path(&left);
            let right_path = bytes_to_path(&right);
            write_frame(
                stdout,
                &IoEvent::DoneCanonicalize {
                    left: wire_result(left_path.canonicalize().map(|p| path_to_bytes(&p))),
                    right: wire_result(right_path.canonicalize().map(|p| path_to_bytes(&p))),
                },
            )?;
        }
        IoRequest::ReadDir {
            path,
            root,
            remaining,
            checkpoint_every,
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let root_path = bytes_to_path(&root);
            let listing = read_dir_request(&path, &root_path, remaining, checkpoint_every, stdout)?;
            write_frame(stdout, &IoEvent::DoneReadDir { listing })?;
        }
        IoRequest::FileBlobHash {
            path,
            hash_kind,
            workdir,
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let mut path = bytes_to_path(&path);
            // Only the helper process may chdir: in-process dispatch shares
            // the caller's CWD (tests / `execute_to`) and must not move it.
            if std::env::var_os(STATUS_IO_WORKER_CAP_ENV).is_some() {
                let workdir = bytes_to_path(&workdir);
                if let Err(error) = std::env::set_current_dir(&workdir) {
                    write_frame(
                        stdout,
                        &IoEvent::DoneHash {
                            hex: WireResult::Err {
                                kind: kind_to_u8(error.kind()),
                                raw_os: error.raw_os_error(),
                            },
                        },
                    )?;
                    return Ok(true);
                }
                // `chdir` resolves symlinks, so the process CWD can differ
                // textually from the request's workdir (stock macOS puts
                // `TMPDIR` under `/var -> private/var`). Attribute lookup
                // anchors on the resolved CWD and drops any path that does not
                // look like its descendant, so an absolute request path in the
                // unresolved spelling would silently lose `.gitattributes` and
                // hash an LFS-tracked file as raw content. Re-anchor the path
                // on the workdir we just entered.
                if let Ok(relative) = path.strip_prefix(&workdir)
                    && !relative.as_os_str().is_empty()
                {
                    path = relative.to_path_buf();
                }
            }
            apply_hash_kind(&hash_kind);
            let result = match crate::command::calc_file_blob_hash(&path) {
                Ok(hash) => WireResult::Ok(hash.to_string()),
                Err(error) => WireResult::Err {
                    kind: kind_to_u8(error.kind()),
                    raw_os: error.raw_os_error(),
                },
            };
            write_frame(stdout, &IoEvent::DoneHash { hex: result })?;
        }
        IoRequest::ReadObjectBlob {
            oid,
            objects_root,
            byte_limit,
            hash_kind,
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            maybe_test_slow_object_read(&oid);
            apply_hash_kind(&hash_kind);
            let objects_root = bytes_to_path(&objects_root);
            write_object_blob_outcome(
                stdout,
                read_object_blob_request(&oid, &objects_root, byte_limit),
            )?;
        }
        IoRequest::MarkerProbe { dir, root } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let dir = bytes_to_path(&dir);
            let root_path = bytes_to_path(&root);
            let (present, err_kind, err_raw_os) = marker_probe_request(&dir, &root_path);
            write_frame(
                stdout,
                &IoEvent::DoneMarker {
                    present,
                    err_kind,
                    err_raw_os,
                },
            )?;
        }
    }
    Ok(true)
}

fn request_root_bytes() -> io::Result<Vec<u8>> {
    STATUS_IO_ROOT_BYTES.with(|slot| {
        if let Some(bytes) = slot.borrow().as_ref() {
            return Ok(bytes.clone());
        }
        let path = crate::utils::util::try_working_dir().map_err(|error| {
            io::Error::other(format!(
                "cannot resolve worktree root for beneath I/O: {error}"
            ))
        })?;
        let bytes = path_to_bytes(&path);
        if bytes.is_empty() {
            return Err(io::Error::other(
                "worktree root resolved empty for beneath I/O",
            ));
        }
        *slot.borrow_mut() = Some(bytes.clone());
        Ok(bytes)
    })
}

thread_local! {
    /// Parent-side worktree root for beneath requests. Resolved once per
    /// status session so `deadline_stat` / `read_dir` do not re-walk the
    /// repository ancestry for every path.
    static STATUS_IO_ROOT_BYTES: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Prime the parent-side worktree-root cache for a status/probe session.
pub(crate) fn prime_status_io_root_cache(root: &Path) {
    let bytes = path_to_bytes(root);
    if bytes.is_empty() {
        return;
    }
    STATUS_IO_ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = Some(bytes);
    });
}

/// Drop the parent-side worktree-root cache at the end of a status session.
pub(crate) fn clear_status_io_root_cache() {
    STATUS_IO_ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// RAII session that primes [`prime_status_io_root_cache`] and clears on drop.
pub(crate) struct StatusIoRootGuard;

impl Drop for StatusIoRootGuard {
    fn drop(&mut self) {
        clear_status_io_root_cache();
    }
}

/// Begin a status I/O session: resolve the worktree root once for all
/// subsequent beneath requests on this thread.
pub(crate) fn begin_status_io_root_session() -> io::Result<StatusIoRootGuard> {
    let path = crate::utils::util::try_working_dir().map_err(|error| {
        io::Error::other(format!(
            "cannot resolve worktree root for beneath I/O: {error}"
        ))
    })?;
    prime_status_io_root_cache(&path);
    Ok(StatusIoRootGuard)
}

fn lstat_request(path: &Path, root: &Path) -> WireResult<CapturedStat> {
    if root.as_os_str().is_empty() {
        return match std::fs::symlink_metadata(path) {
            Ok(meta) => WireResult::Ok(CapturedStat::from_metadata(&meta)),
            Err(error) => WireResult::Err {
                kind: kind_to_u8(error.kind()),
                raw_os: error.raw_os_error(),
            },
        };
    }
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => {
            return WireResult::Err {
                kind: kind_to_u8(io::ErrorKind::Other),
                raw_os: None,
            };
        }
    };
    match crate::utils::beneath::open_root(root)
        .and_then(|fd| crate::utils::beneath::lstat_beneath(&fd, rel))
    {
        Ok(raw) => WireResult::Ok(CapturedStat::from_raw_lstat(&raw)),
        Err(error) => WireResult::Err {
            kind: kind_to_u8(error.kind()),
            raw_os: error.raw_os_error(),
        },
    }
}

fn marker_probe_request(dir: &Path, root: &Path) -> (Option<bool>, Option<u8>, Option<i32>) {
    if root.as_os_str().is_empty() {
        let mut present = false;
        for marker in [crate::utils::util::ROOT_DIR, crate::utils::util::GIT_DIR] {
            match dir.join(marker).symlink_metadata() {
                Ok(_) => {
                    present = true;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return (None, Some(kind_to_u8(error.kind())), error.raw_os_error());
                }
            }
        }
        return (Some(present), None, None);
    }
    let rel = match dir.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return (None, Some(kind_to_u8(io::ErrorKind::Other)), None),
    };
    match crate::utils::beneath::open_root(root)
        .and_then(|fd| crate::utils::beneath::marker_present_beneath(&fd, rel))
    {
        Ok(present) => (Some(present), None, None),
        Err(error) => (None, Some(kind_to_u8(error.kind())), error.raw_os_error()),
    }
}

fn read_dir_request(
    path: &Path,
    root: &Path,
    remaining: usize,
    checkpoint_every: u32,
    stdout: &mut impl Write,
) -> io::Result<ReadDirListing> {
    let mut listing = ReadDirListing {
        entries: Vec::new(),
        error_kinds: Vec::new(),
        taken: 0,
        hit_cap: false,
        timed_out: false,
    };
    if root.as_os_str().is_empty() {
        match std::fs::read_dir(path) {
            Err(error) => {
                listing
                    .error_kinds
                    .push((kind_to_u8(error.kind()), error.raw_os_error()));
            }
            Ok(reader) => {
                emit_read_dir(
                    reader.map(|entry| entry.map(|entry| Dirent::from_dir_entry(&entry))),
                    path,
                    remaining,
                    checkpoint_every,
                    &mut listing,
                    stdout,
                )?;
            }
        }
        listing.entries.clear();
        return Ok(listing);
    }
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => {
            listing
                .error_kinds
                .push((kind_to_u8(io::ErrorKind::Other), None));
            listing.entries.clear();
            return Ok(listing);
        }
    };
    match crate::utils::beneath::open_root(root)
        .and_then(|fd| crate::utils::beneath::open_beneath(&fd, rel))
        .and_then(crate::utils::beneath::read_dir_fd)
    {
        Err(error) => {
            listing
                .error_kinds
                .push((kind_to_u8(error.kind()), error.raw_os_error()));
        }
        Ok(reader) => {
            emit_read_dir(
                reader.map(|entry| entry.map(|entry| Dirent::from_fd_dirent(&entry))),
                path,
                remaining,
                checkpoint_every,
                &mut listing,
                stdout,
            )?;
        }
    }
    listing.entries.clear();
    Ok(listing)
}

fn emit_read_dir<I>(
    reader: I,
    #[cfg_attr(not(debug_assertions), allow(unused_variables))] path: &Path,
    remaining: usize,
    checkpoint_every: u32,
    listing: &mut ReadDirListing,
    stdout: &mut impl Write,
) -> io::Result<()>
where
    I: Iterator<Item = io::Result<Dirent>>,
{
    let mut seq = 0u64;
    let mut records = 0u64;
    let every = checkpoint_every.max(1);
    #[cfg(debug_assertions)]
    let mut injected_notfound = false;
    for entry in reader {
        #[cfg(debug_assertions)]
        let entry = if !injected_notfound
            && std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
            && std::env::var("LIBRA_TEST_READDIR_ENTRY_NOTFOUND_DIR")
                .is_ok_and(|target| path.ends_with(&target))
        {
            injected_notfound = true;
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "injected vanished entry",
            ))
        } else {
            entry
        };
        listing.taken += 1;
        if listing.taken > remaining {
            listing.hit_cap = true;
            break;
        }
        match entry {
            Ok(dirent) => {
                write_frame(stdout, &IoEvent::RecordDirent(dirent))?;
                records += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                write_frame(
                    stdout,
                    &IoEvent::RecordError {
                        kind: kind_to_u8(error.kind()),
                        raw_os: error.raw_os_error(),
                    },
                )?;
                listing
                    .error_kinds
                    .push((kind_to_u8(error.kind()), error.raw_os_error()));
                break;
            }
        }
        if records > 0 && (records as u32).is_multiple_of(every) {
            seq += 1;
            write_frame(stdout, &IoEvent::Checkpoint { seq, records })?;
            maybe_test_kill_after_checkpoint(seq);
        }
        #[cfg(debug_assertions)]
        if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
            && std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_DIR")
                .is_ok_and(|target| path.ends_with(&target))
        {
            let kind = match std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_KIND").as_deref() {
                Ok("timedout") => io::ErrorKind::TimedOut,
                _ => io::ErrorKind::Other,
            };
            write_frame(
                stdout,
                &IoEvent::RecordError {
                    kind: kind_to_u8(kind),
                    raw_os: None,
                },
            )?;
            listing.error_kinds.push((kind_to_u8(kind), None));
            break;
        }
    }
    Ok(())
}

fn apply_hash_kind(kind: &str) {
    match kind {
        "sha256" => git_internal::hash::set_hash_kind(git_internal::hash::HashKind::Sha256),
        _ => git_internal::hash::set_hash_kind(git_internal::hash::HashKind::Sha1),
    }
}

fn maybe_test_kill_after_checkpoint(seq: u64) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return;
    }
    let Ok(wanted) = std::env::var("LIBRA_TEST_STATUS_IO_KILL_AFTER_CHECKPOINT") else {
        return;
    };
    let Ok(wanted) = wanted.parse::<u64>() else {
        return;
    };
    if seq == wanted {
        std::process::exit(99);
    }
}

/// Debug seam: sleep before a local object read so WIO-03 can prove the
/// parent kills the helper when the batch deadline elapses mid-read.
fn maybe_test_slow_object_read(oid: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return;
    }
    let Ok(ms) = std::env::var("LIBRA_TEST_SLOW_OBJECT_READ_MS") else {
        return;
    };
    let Ok(ms) = ms.parse::<u64>() else {
        return;
    };
    if let Ok(wanted) = std::env::var("LIBRA_TEST_SLOW_OBJECT_READ_OID")
        && !wanted.is_empty()
        && wanted != oid
    {
        return;
    }
    std::thread::sleep(Duration::from_millis(ms));
}

fn read_object_blob_request(
    oid: &str,
    objects_root: &Path,
    byte_limit: u64,
) -> Result<Vec<u8>, ObjectBlobStatus> {
    use crate::utils::client_storage::{ClientStorage, ObjectReadFailure};

    let Ok(hash) = oid.parse::<git_internal::hash::ObjectHash>() else {
        return Err(ObjectBlobStatus::Failed);
    };
    // Local-only + alternates, no directory creation / remote hydrate
    // (WIO-03 security AC).
    let storage = ClientStorage::init_local_existing_with_alternates(objects_root.to_path_buf());
    match storage.get_with_limit(&hash, byte_limit) {
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(match ClientStorage::classify_read_failure(&error) {
            ObjectReadFailure::Missing => ObjectBlobStatus::Missing,
            ObjectReadFailure::Corrupt => ObjectBlobStatus::Corrupt,
            ObjectReadFailure::Unavailable => ObjectBlobStatus::Unavailable,
            ObjectReadFailure::TooLarge => ObjectBlobStatus::TooLarge,
            ObjectReadFailure::Other => ObjectBlobStatus::Failed,
        }),
    }
}

fn write_object_blob_outcome(
    writer: &mut impl Write,
    outcome: Result<Vec<u8>, ObjectBlobStatus>,
) -> io::Result<()> {
    match outcome {
        Ok(bytes) => {
            // Decide the over-cap case BEFORE the Ok header goes out: a
            // blob past FRAME_CAP used to fail inside `write_raw_frame`
            // AFTER `Ok` was already written, leaving the parent blocked on
            // a raw frame that never arrives (indistinguishable from a hung
            // read until the deadline kill). Reporting `TooLarge` up front
            // keeps the stream consistent and lets callers with a byte
            // limit above the frame cap (diff, W5-09) fall back promptly.
            if bytes.len() > FRAME_CAP {
                return write_frame(
                    writer,
                    &IoEvent::DoneObjectBlob {
                        status: ObjectBlobStatus::TooLarge,
                        bytes: None,
                    },
                );
            }
            write_frame(
                writer,
                &IoEvent::DoneObjectBlob {
                    status: ObjectBlobStatus::Ok,
                    bytes: None,
                },
            )?;
            write_raw_frame(writer, &bytes)
        }
        Err(status) => write_frame(
            writer,
            &IoEvent::DoneObjectBlob {
                status,
                bytes: None,
            },
        ),
    }
}

fn write_raw_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > FRAME_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker binary frame too large",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn read_raw_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > FRAME_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker binary frame length invalid",
        ));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut payload)?;
    }
    Ok(payload)
}

fn current_hash_kind() -> String {
    match git_internal::hash::get_hash_kind() {
        git_internal::hash::HashKind::Sha256 => "sha256".to_string(),
        _ => "sha1".to_string(),
    }
}

struct WorkerProc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct Pool {
    idle: Mutex<Vec<WorkerProc>>,
    pending: Mutex<VecDeque<Arc<Job>>>,
    ready: Condvar,
    token: String,
}

struct Job {
    request: Mutex<Option<IoRequest>>,
    path_key: Vec<u8>,
    /// Wall-clock bound captured at submit (`now + timeout`).
    deadline: Instant,
    /// Per-stdout-wait window. Status walks use this as a no-progress timeout
    /// (fresh each frame). Object reads set `absolute` and share `deadline`
    /// across queue + every wait so a busy pool cannot stretch the budget.
    window: Duration,
    absolute: bool,
    result_tx: Mutex<Option<std::sync::mpsc::SyncSender<JobOutcome>>>,
    cancelled: AtomicBool,
}

enum JobOutcome {
    Events(Vec<IoEvent>),
    Timeout,
    Failed,
}

static POOL: OnceLock<Arc<Pool>> = OnceLock::new();
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
static DISPATCHERS_STARTED: AtomicUsize = AtomicUsize::new(0);

fn pool() -> &'static Arc<Pool> {
    POOL.get_or_init(|| {
        let token = format!(
            "{:016x}{:016x}",
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            std::process::id() as u64
        );
        let pool = Arc::new(Pool {
            idle: Mutex::new(Vec::new()),
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            token,
        });
        let mut started = 0usize;
        for index in 0..MAX_INFLIGHT {
            let pool = Arc::clone(&pool);
            if std::thread::Builder::new()
                .name(format!("libra-status-io-{index}"))
                .spawn(move || dispatcher_loop(pool))
                .is_ok()
            {
                started += 1;
            }
        }
        DISPATCHERS_STARTED.store(started, Ordering::SeqCst);
        pool
    })
}

fn spawn_worker(token: &str) -> io::Result<WorkerProc> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg(STATUS_IO_WORKER_ARG)
        .env(STATUS_IO_WORKER_CAP_ENV, token)
        .env(STATUS_IO_WORKER_PPID_ENV, std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // Own process group so timeout kill(-pid) cannot hit status;
            // PDEATHSIG so a killed/exiting parent still reaps a hung helper.
            // SAFETY: runs in the child after fork, before exec. Only
            // async-signal-safe calls (prctl / getppid / _exit).
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() == 1 {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("status io worker missing stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("status io worker missing stdout"))?;
    let mut stdout = BufReader::new(stdout);
    let ready: IoEvent = match read_frame(&mut stdout) {
        Ok(event) => event,
        Err(error) => {
            kill_pid(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    if !matches!(ready, IoEvent::Ready) {
        kill_pid(child.id());
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("status io worker handshake failed"));
    }
    Ok(WorkerProc {
        child,
        stdin,
        stdout,
    })
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                let _ = TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

fn kill_worker(worker: &mut WorkerProc) {
    kill_pid(worker.child.id());
    let _ = worker.child.kill();
    let _ = worker.child.wait();
}

fn submit(request: IoRequest, path_key: Vec<u8>, timeout: Duration) -> Result<Vec<IoEvent>, ()> {
    submit_with_clock(request, path_key, timeout, false)
}

fn submit_absolute(
    request: IoRequest,
    path_key: Vec<u8>,
    timeout: Duration,
) -> Result<Vec<IoEvent>, ()> {
    submit_with_clock(request, path_key, timeout, true)
}

fn submit_with_clock(
    request: IoRequest,
    path_key: Vec<u8>,
    timeout: Duration,
    absolute: bool,
) -> Result<Vec<IoEvent>, ()> {
    let pool = pool();
    if DISPATCHERS_STARTED.load(Ordering::SeqCst) == 0 {
        return Err(());
    }
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::sync_channel(1);
    let job = Arc::new(Job {
        request: Mutex::new(Some(request)),
        path_key,
        deadline,
        window: timeout,
        absolute,
        result_tx: Mutex::new(Some(tx)),
        cancelled: AtomicBool::new(false),
    });
    {
        let mut pending = pool
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len() >= MAX_PENDING {
            return Err(());
        }
        let idx = pending
            .iter()
            .position(|existing| existing.path_key > job.path_key)
            .unwrap_or(pending.len());
        pending.insert(idx, Arc::clone(&job));
        pool.ready.notify_one();
    }
    // CLI helpers: progressing `read_dir` may outlive one window; wait long
    // enough. Absolute object reads finish or kill within `deadline`.
    // In-process fallback cannot kill a hung syscall — recycle the caller
    // after the op deadline instead of blocking `submit` forever.
    let wait = if helper_exe_is_cli() && !absolute {
        Duration::from_secs(24 * 60 * 60)
    } else {
        deadline
            .saturating_duration_since(Instant::now())
            .saturating_add(Duration::from_secs(1))
    };
    match rx.recv_timeout(wait) {
        Ok(JobOutcome::Events(events)) => Ok(events),
        _ => {
            job.cancelled.store(true, Ordering::SeqCst);
            {
                let mut pending = pool
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(idx) = pending
                    .iter()
                    .position(|existing| Arc::ptr_eq(existing, &job))
                {
                    pending.remove(idx);
                }
            }
            let _ = job
                .request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            Err(())
        }
    }
}

fn dispatcher_loop(pool: Arc<Pool>) {
    loop {
        let job = {
            let mut pending = pool
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(job) = pending.pop_front() {
                    if job.cancelled.load(Ordering::SeqCst) {
                        continue;
                    }
                    break job;
                }
                pending = pool
                    .ready
                    .wait(pending)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        run_job(&pool.token, job);
    }
}

fn helper_exe_is_cli() -> bool {
    static CLI: OnceLock<bool> = OnceLock::new();
    *CLI.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        // Cargo test harnesses live in `target/.../deps/`; the installed /
        // `cargo run` CLI is `…/libra` (or `libra.exe`).
        let in_deps = exe
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name == "deps");
        if in_deps {
            return false;
        }
        exe.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "libra" || name.eq_ignore_ascii_case("libra.exe"))
    })
}

fn run_job(token: &str, job: Arc<Job>) {
    if job.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let request = job
        .request
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(request) = request else {
        return;
    };
    if job.cancelled.load(Ordering::SeqCst) {
        return;
    }
    if job.absolute && Instant::now() >= job.deadline {
        if let Some(tx) = job
            .result_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = tx.send(JobOutcome::Timeout);
        }
        return;
    }
    let outcome = match take_worker(token) {
        // CLI helper spawn/handshake failed (EMFILE, process limit). Do not
        // fall back to an unkillable in-process syscall — that would pin a
        // dispatcher forever and exhaust the pool. Absolute object reads
        // (WIO-03) are the same: never `run_in_process` them on a pool
        // thread. Library/test binaries keep in-process only for relative
        // (no-progress) probe opcodes (R0).
        Err(()) if helper_exe_is_cli() || job.absolute => JobOutcome::Timeout,
        Err(()) => run_in_process(request),
        Ok(mut worker) => {
            let (events, timed_out, reuse) = drive_worker(
                &mut worker,
                token,
                request,
                job.deadline,
                job.window,
                job.absolute,
            );
            if timed_out || !reuse {
                kill_worker(&mut worker);
            } else {
                recycle_worker(worker);
            }
            if events.is_empty() {
                if timed_out {
                    JobOutcome::Timeout
                } else {
                    JobOutcome::Failed
                }
            } else {
                JobOutcome::Events(events)
            }
        }
    };
    if job.cancelled.load(Ordering::SeqCst) {
        return;
    }
    if let Some(tx) = job
        .result_tx
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        let _ = tx.send(outcome);
    }
}

/// Library / in-process callers (`status::execute_to`, `cargo test` unit
/// binaries) cannot spawn the CLI helper. Run the opcode on this dispatcher
/// thread (already one of the 8 bounded slots). Hung syscalls remain
/// unkillable here; WIO-01 killability applies to the `libra` CLI worker.
fn run_in_process(request: IoRequest) -> JobOutcome {
    let mut buf = Vec::new();
    match handle_request(request, &mut buf)
        .ok()
        .and_then(|_| parse_event_frames(&buf))
    {
        Some(events) if !events.is_empty() => JobOutcome::Events(events),
        _ => JobOutcome::Failed,
    }
}

fn parse_event_frames(mut data: &[u8]) -> Option<Vec<IoEvent>> {
    let mut events = Vec::new();
    while !data.is_empty() {
        if data.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        data = &data[4..];
        if len == 0 || len > FRAME_CAP || data.len() < len {
            return None;
        }
        let event: IoEvent = serde_json::from_slice(&data[..len]).ok()?;
        data = &data[len..];
        let event = if let IoEvent::DoneObjectBlob {
            status: ObjectBlobStatus::Ok,
            bytes: None,
        } = &event
        {
            if data.len() < 4 {
                return None;
            }
            let raw_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if raw_len > FRAME_CAP || data.len() < raw_len {
                return None;
            }
            let bytes = data[..raw_len].to_vec();
            data = &data[raw_len..];
            IoEvent::DoneObjectBlob {
                status: ObjectBlobStatus::Ok,
                bytes: Some(bytes),
            }
        } else {
            event
        };
        events.push(event);
    }
    Some(events)
}

fn take_worker(token: &str) -> Result<WorkerProc, ()> {
    if !helper_exe_is_cli() {
        return Err(());
    }
    {
        let mut idle = pool()
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(worker) = idle.pop() {
            return Ok(worker);
        }
    }
    spawn_worker(token).map_err(|_| ())
}

fn recycle_worker(worker: WorkerProc) {
    let mut idle = pool()
        .idle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if idle.len() < MAX_INFLIGHT {
        idle.push(worker);
    } else {
        let mut worker = worker;
        kill_worker(&mut worker);
    }
}

fn wait_stdout_readable(worker: &mut WorkerProc, timeout: Duration) -> io::Result<()> {
    if !worker.stdout.buffer().is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = worker.stdout.get_ref().as_raw_fd();
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        loop {
            let n = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "status io worker timeout",
                ));
            }
            if pollfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            return Err(io::Error::other("status io worker poll"));
        }
    }
    #[cfg(windows)]
    {
        // Anonymous pipe handles are not waitable synchronization objects;
        // `WaitForSingleObject` returns WAIT_FAILED. PeekNamedPipe reports
        // pending bytes (or ERROR_BROKEN_PIPE on EOF).
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::{
            Foundation::{ERROR_BROKEN_PIPE, HANDLE},
            System::Pipes::PeekNamedPipe,
        };
        const POLL_SLICE: Duration = Duration::from_millis(5);
        let deadline = std::time::Instant::now() + timeout;
        let handle = worker.stdout.get_ref().as_raw_handle() as HANDLE;
        loop {
            let mut avail: u32 = 0;
            let ok = unsafe {
                PeekNamedPipe(
                    handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Ok(());
                }
                return Err(error);
            }
            if avail > 0 {
                return Ok(());
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "status io worker timeout",
                ));
            }
            std::thread::sleep(POLL_SLICE.min(deadline.saturating_duration_since(now)));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = timeout;
        Ok(())
    }
}

fn drive_worker(
    worker: &mut WorkerProc,
    token: &str,
    request: IoRequest,
    deadline: Instant,
    window: Duration,
    absolute: bool,
) -> (Vec<IoEvent>, bool, bool) {
    if write_request(&mut worker.stdin, token, request).is_err() {
        return (Vec::new(), false, false);
    }
    let mut events = Vec::new();
    loop {
        let wait = if absolute {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return (events, true, false);
            }
            remaining
        } else {
            // No-progress: each frame gets a fresh window so a wide
            // progressing `read_dir` is not cut by an absolute job clock.
            window
        };
        if wait_stdout_readable(worker, wait).is_err() {
            return (events, true, false);
        }
        let event = match read_frame::<IoEvent>(&mut worker.stdout) {
            Ok(event) => event,
            Err(error)
                if error.kind() == io::ErrorKind::TimedOut
                    || error.kind() == io::ErrorKind::WouldBlock =>
            {
                return (events, true, false);
            }
            Err(_) => {
                let timed_out = !events.is_empty();
                return (events, timed_out, false);
            }
        };
        let reuse = !matches!(event, IoEvent::Error { .. });
        // Ok payloads travel as a trailing length-prefixed binary frame
        // (not base64 in JSON) so a 2 MiB blob stays within the ≤20%
        // wire-overhead budget (WIO-03).
        let event = if let IoEvent::DoneObjectBlob {
            status: ObjectBlobStatus::Ok,
            bytes: None,
        } = &event
        {
            let wait = if absolute {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return (events, true, false);
                }
                remaining
            } else {
                window
            };
            if wait_stdout_readable(worker, wait).is_err() {
                return (events, true, false);
            }
            match read_raw_frame(&mut worker.stdout) {
                Ok(bytes) => IoEvent::DoneObjectBlob {
                    status: ObjectBlobStatus::Ok,
                    bytes: Some(bytes),
                },
                Err(_) => return (events, true, false),
            }
        } else {
            event
        };
        let done = matches!(
            event,
            IoEvent::DoneStat { .. }
                | IoEvent::DoneCanonicalize { .. }
                | IoEvent::DoneReadDir { .. }
                | IoEvent::DoneHash { .. }
                | IoEvent::DoneObjectBlob { .. }
                | IoEvent::DoneMarker { .. }
                | IoEvent::Error { .. }
        );
        events.push(event);
        if done {
            return (events, false, reuse);
        }
    }
}

fn write_request(writer: &mut impl Write, token: &str, request: IoRequest) -> io::Result<()> {
    let wrapped = CapRequest {
        cap: token.to_string(),
        request,
    };
    let payload = serde_json::to_vec(&wrapped)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > FRAME_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker request too large",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

pub(crate) fn deadline_stat(path: &Path) -> Result<io::Result<CapturedStat>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let events = submit(
        IoRequest::SymlinkMetadata {
            path: path_to_bytes(path),
            root,
        },
        path_to_bytes(path),
        crate::command::status_probe::io_op_timeout(),
    )?;
    for event in events {
        if let IoEvent::DoneStat { result } = event {
            return Ok(unwrap_wire(result));
        }
    }
    Err(())
}

pub(crate) fn deadline_canonicalize_pair(
    left: &Path,
    right: &Path,
) -> Result<(io::Result<PathBuf>, io::Result<PathBuf>), ()> {
    let events = submit(
        IoRequest::CanonicalizePair {
            left: path_to_bytes(left),
            right: path_to_bytes(right),
        },
        path_to_bytes(left),
        crate::command::status_probe::io_op_timeout(),
    )?;
    for event in events {
        if let IoEvent::DoneCanonicalize { left, right } = event {
            return Ok((
                unwrap_wire(left).map(|bytes| bytes_to_path(&bytes)),
                unwrap_wire(right).map(|bytes| bytes_to_path(&bytes)),
            ));
        }
    }
    Err(())
}

pub(crate) fn deadline_read_dir(
    path: &Path,
    remaining: usize,
    progress: &AtomicUsize,
) -> Result<io::Result<ReadDirListing>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let events = submit(
        IoRequest::ReadDir {
            path: path_to_bytes(path),
            root,
            remaining,
            checkpoint_every: 32,
        },
        path_to_bytes(path),
        crate::command::status_probe::io_op_timeout(),
    );
    match events {
        Err(()) => Err(()),
        Ok(events) => {
            let mut partial = ReadDirListing {
                entries: Vec::new(),
                error_kinds: Vec::new(),
                taken: 0,
                hit_cap: false,
                timed_out: false,
            };
            let mut complete = false;
            for event in events {
                match event {
                    IoEvent::RecordDirent(dirent) => {
                        progress.fetch_add(1, Ordering::SeqCst);
                        partial.taken += 1;
                        partial.entries.push(dirent);
                    }
                    IoEvent::RecordError { kind, raw_os } => {
                        progress.fetch_add(1, Ordering::SeqCst);
                        partial.taken += 1;
                        partial.error_kinds.push((kind, raw_os));
                    }
                    IoEvent::DoneReadDir { listing } => {
                        if listing.entries.is_empty()
                            && listing.error_kinds.len() == 1
                            && listing.taken == 0
                            && partial.taken == 0
                        {
                            let (kind, raw_os) = listing.error_kinds[0];
                            return Ok(Err(io_from_wire(kind, raw_os)));
                        }
                        if !listing.entries.is_empty() {
                            partial.entries = listing.entries;
                        }
                        if !listing.error_kinds.is_empty() {
                            partial.error_kinds = listing.error_kinds;
                        }
                        partial.hit_cap = listing.hit_cap;
                        if listing.taken > partial.taken {
                            partial.taken = listing.taken;
                        }
                        complete = true;
                    }
                    _ => {}
                }
            }
            if complete {
                Ok(Ok(partial))
            } else if partial.taken > 0 || !partial.error_kinds.is_empty() {
                partial.timed_out = true;
                Ok(Ok(partial))
            } else {
                Err(())
            }
        }
    }
}

/// Use the worker-side `file_type()` when present; otherwise one killable
/// `deadline_stat` for that single name (DT_UNKNOWN / `file_type` error).
pub(crate) fn deadline_dirent_kind(
    path: &Path,
    dirent: &Dirent,
) -> Result<io::Result<DirentKind>, ()> {
    if dirent.type_ok {
        return Ok(Ok(DirentKind {
            is_dir: dirent.is_dir,
            is_file: dirent.is_file,
            is_symlink: dirent.is_symlink,
        }));
    }
    match deadline_stat(path) {
        Err(()) => Err(()),
        Ok(Err(error)) => Ok(Err(error)),
        Ok(Ok(stat)) => Ok(Ok(DirentKind {
            is_dir: stat.is_dir(),
            is_file: stat.is_file(),
            is_symlink: stat.is_symlink(),
        })),
    }
}

pub(crate) fn deadline_file_blob_hash(
    path: &Path,
    workdir: &Path,
) -> Result<io::Result<git_internal::hash::ObjectHash>, ()> {
    let events = submit(
        IoRequest::FileBlobHash {
            path: path_to_bytes(path),
            hash_kind: current_hash_kind(),
            workdir: path_to_bytes(workdir),
        },
        path_to_bytes(path),
        crate::command::status_probe::io_op_timeout(),
    )?;
    for event in events {
        if let IoEvent::DoneHash { hex } = event {
            return Ok(unwrap_wire(hex).and_then(|hex| {
                hex.parse::<git_internal::hash::ObjectHash>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
            }));
        }
    }
    Err(())
}

/// Outcome of a killable local object-store read (WIO-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectBlobOutcome {
    Bytes(Vec<u8>),
    Missing,
    Corrupt,
    Unavailable,
    TooLarge,
    Failed,
}

/// Read a peeled OID from `objects_root` under `timeout`. On the `libra` CLI,
/// a hung store read kills the helper process group and returns `Err(())` so
/// the caller can map the edge to a metadata skip without stalling the batch.
///
/// Library / `cargo test` harness binaries cannot spawn the CLI helper. Those
/// callers read locally on the **caller** thread (R0 mid-read hang semantics)
/// instead of occupying a dispatcher slot with an unkillable syscall
/// (WIO-03 Codex: pool-slot leak under hung mounts).
pub(crate) fn deadline_read_object_blob(
    oid: &git_internal::hash::ObjectHash,
    objects_root: &Path,
    byte_limit: u64,
    timeout: Duration,
) -> Result<ObjectBlobOutcome, ()> {
    if timeout.is_zero() {
        return Err(());
    }
    if !helper_exe_is_cli() {
        return Ok(object_blob_outcome_from_status(read_object_blob_request(
            &oid.to_string(),
            objects_root,
            byte_limit,
        )));
    }
    let oid_hex = oid.to_string();
    let events = submit_absolute(
        IoRequest::ReadObjectBlob {
            oid: oid_hex.clone(),
            objects_root: path_to_bytes(objects_root),
            byte_limit,
            hash_kind: current_hash_kind(),
        },
        oid_hex.into_bytes(),
        timeout,
    )?;
    for event in events {
        if let IoEvent::DoneObjectBlob { status, bytes } = event {
            return Ok(match status {
                ObjectBlobStatus::Ok => match bytes {
                    Some(bytes) => ObjectBlobOutcome::Bytes(bytes),
                    // Wire claimed Ok but the trailing binary frame was
                    // lost — treat as corrupt rather than silently empty.
                    None => ObjectBlobOutcome::Corrupt,
                },
                other => object_blob_outcome_from_status(Err(other)),
            });
        }
    }
    Err(())
}

fn object_blob_outcome_from_status(
    outcome: Result<Vec<u8>, ObjectBlobStatus>,
) -> ObjectBlobOutcome {
    match outcome {
        Ok(bytes) => ObjectBlobOutcome::Bytes(bytes),
        Err(ObjectBlobStatus::Ok) => ObjectBlobOutcome::Corrupt,
        Err(ObjectBlobStatus::Missing) => ObjectBlobOutcome::Missing,
        Err(ObjectBlobStatus::Corrupt) => ObjectBlobOutcome::Corrupt,
        Err(ObjectBlobStatus::Unavailable) => ObjectBlobOutcome::Unavailable,
        Err(ObjectBlobStatus::TooLarge) => ObjectBlobOutcome::TooLarge,
        Err(ObjectBlobStatus::Failed) => ObjectBlobOutcome::Failed,
    }
}

pub(crate) fn deadline_marker_probe(dir: &Path) -> Result<Result<bool, io::Error>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let events = submit(
        IoRequest::MarkerProbe {
            dir: path_to_bytes(dir),
            root,
        },
        path_to_bytes(dir),
        crate::command::status_probe::io_op_timeout(),
    )?;
    for event in events {
        if let IoEvent::DoneMarker {
            present,
            err_kind,
            err_raw_os,
        } = event
        {
            if let Some(kind) = err_kind {
                return Ok(Err(io_from_wire(kind, err_raw_os)));
            }
            return Ok(Ok(present.unwrap_or(false)));
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn captured_stat_round_trips_a_regular_file() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let meta = std::fs::metadata(&manifest).expect("Cargo.toml");
        let captured = super::CapturedStat::from_metadata(&meta);
        assert!(captured.is_file());
        assert!(!captured.is_dir());
        assert!(!captured.is_symlink());
        assert!(captured.len() > 0);
    }

    #[test]
    fn dirent_captures_file_type_from_readdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("a.txt"), b"x").expect("file");
        std::fs::create_dir(tmp.path().join("d")).expect("dir");
        let mut seen_file = false;
        let mut seen_dir = false;
        for entry in std::fs::read_dir(tmp.path()).expect("read_dir") {
            let dirent = super::Dirent::from_dir_entry(&entry.expect("entry"));
            assert!(dirent.type_ok, "readdir file_type must succeed here");
            seen_file |= dirent.is_file;
            seen_dir |= dirent.is_dir;
        }
        assert!(seen_file && seen_dir);
    }

    #[test]
    #[serial_test::serial]
    fn file_blob_hash_helper_uses_request_workdir_not_spawn_cwd() {
        use std::ffi::OsString;

        use git_internal::internal::object::blob::Blob;

        struct CapEnvGuard(Option<OsString>);
        impl Drop for CapEnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match &self.0 {
                        Some(value) => {
                            std::env::set_var(super::STATUS_IO_WORKER_CAP_ENV, value);
                        }
                        None => std::env::remove_var(super::STATUS_IO_WORKER_CAP_ENV),
                    }
                }
            }
        }
        struct HashKindGuard(git_internal::hash::HashKind);
        impl Drop for HashKindGuard {
            fn drop(&mut self) {
                git_internal::hash::set_hash_kind(self.0);
            }
        }

        fn fake_repo(label: &str) -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{label}: {error}"));
            let libra = dir.path().join(".libra");
            std::fs::create_dir(&libra).unwrap_or_else(|error| panic!("{label} .libra: {error}"));
            std::fs::write(libra.join("libra.db"), b"").expect("libra.db");
            dir
        }

        let repo_a = fake_repo("A");
        let repo_b = fake_repo("B");
        std::fs::write(repo_b.path().join(".gitattributes"), "*.bin filter=lfs\n")
            .expect("gitattributes");
        let payload = b"payload\n";
        let file_b = repo_b.path().join("tracked.bin");
        std::fs::write(&file_b, payload).expect("tracked.bin");

        let _cwd = crate::utils::test::ChangeDirGuard::new(repo_a.path());
        let _cap = CapEnvGuard(std::env::var_os(super::STATUS_IO_WORKER_CAP_ENV));
        let _hash = HashKindGuard(git_internal::hash::get_hash_kind());
        unsafe {
            std::env::set_var(super::STATUS_IO_WORKER_CAP_ENV, "test-cap");
        }

        let request = super::IoRequest::FileBlobHash {
            path: super::path_to_bytes(&file_b),
            hash_kind: "sha1".to_string(),
            workdir: super::path_to_bytes(repo_b.path()),
        };
        let mut buf = Vec::new();
        super::handle_request(request, &mut buf).expect("handle_request");

        let events = super::parse_event_frames(&buf).expect("frames");
        let hex = events.into_iter().find_map(|event| match event {
            super::IoEvent::DoneHash {
                hex: super::WireResult::Ok(hex),
            } => Some(hex),
            _ => None,
        });
        let hex = hex.expect("DoneHash ok");
        let (pointer, _) = crate::utils::lfs::generate_pointer_file(&file_b);
        let lfs_hash = Blob::from_content(&pointer).id.to_string();
        let content_hash = Blob::from_content_bytes(payload.to_vec()).id.to_string();
        assert_eq!(
            hex, lfs_hash,
            "helper must hash via B's LFS attrs, not spawn CWD A"
        );
        assert_ne!(lfs_hash, content_hash, "sanity: LFS pointer ≠ content");
    }
}
