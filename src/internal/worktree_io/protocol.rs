//! Read-only wire protocol and filesystem capabilities for status I/O.
//!
//! This module intentionally has no dependency on `crate::command`.  The
//! command layer owns scheduling and user-facing policy; this module owns the
//! serializable request/event contract, framing, path encoding, and the two
//! distinct read-only filesystem capabilities used by that contract.

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Maximum JSON or binary payload accepted by either side of the protocol.
pub(crate) const FRAME_CAP: usize = 8 * 1024 * 1024;

/// A sealed, canonical worktree root. Requests must carry a relative path
/// checked by [`WorktreeRootCapability::relative`] before beneath I/O opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeRootCapability {
    root: PathBuf,
}

impl WorktreeRootCapability {
    /// Seal an existing, non-symlink directory as a worktree root.
    pub(crate) fn seal(root: &Path) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree capability root must not be a symlink",
            ));
        }
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "worktree capability root must be a directory",
            ));
        }
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Validate and retain a strictly relative path. Empty, `.`/`..`,
    /// absolute, prefixed, and duplicate-separator paths are rejected so a
    /// serialized request cannot smuggle lexical normalization into a lookup.
    pub(crate) fn relative(&self, path: &Path) -> io::Result<PathBuf> {
        validate_relative_path(path)?;
        Ok(path.to_path_buf())
    }

    /// Validate a path that may designate the sealed root itself. The empty
    /// relative path is the wire representation of the root for directory
    /// listing and marker probes; every other path remains strictly relative.
    pub(crate) fn relative_or_root(&self, path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Ok(PathBuf::new());
        }
        self.relative(path)
    }

    /// Resolve a validated relative path for diagnostics or APIs that need a
    /// pathname. Actual reads must still use `utils::beneath` with the sealed
    /// root descriptor to close symlink-swap/TOCTOU races.
    pub(crate) fn resolve(&self, relative: &Path) -> io::Result<PathBuf> {
        Ok(self.root.join(self.relative_or_root(relative)?))
    }
}

/// A separately typed, local-only object-store root capability. It cannot be
/// passed to worktree path APIs, preventing accidental cross-capability reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectStoreCapability {
    root: PathBuf,
}

impl ObjectStoreCapability {
    /// Seal an existing object-store directory without creating or hydrating
    /// anything. The caller maps a missing root to an unavailable read.
    pub(crate) fn seal(root: &Path) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "object-store capability root must not be a symlink",
            ));
        }
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "object-store capability root must be a directory",
            ));
        }
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Return the loose-object pathname for an OID without opening it. This
    /// is intentionally read-only and accepts only canonical SHA-1/SHA-256
    /// hexadecimal object names.
    #[allow(dead_code)]
    pub(crate) fn object_path(&self, oid: &str) -> io::Result<PathBuf> {
        if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "object-store request must contain a 40- or 64-digit hexadecimal OID",
            ));
        }
        Ok(self.root.join(&oid[..2]).join(&oid[2..]))
    }
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() || has_empty_component(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree request path must be a non-empty canonical relative path",
        ));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree request path must not contain '.', '..', or a path prefix",
            ));
        }
    }
    Ok(())
}

fn has_empty_component(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        bytes.split(|byte| *byte == b'/').any(<[u8]>::is_empty)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        units
            .split(|unit| *unit == u16::from(b'\\') || *unit == u16::from(b'/'))
            .any(<[u16]>::is_empty)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

/// Serializable worktree stat (Metadata cannot cross the process boundary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedStat {
    pub(crate) is_symlink: bool,
    pub(crate) is_dir: bool,
    pub(crate) is_file: bool,
    pub(crate) len: u64,
    pub(crate) mode: u32,
    pub(crate) ctime_sec: i64,
    pub(crate) ctime_nsec: i64,
    pub(crate) mtime_sec: i64,
    pub(crate) mtime_nsec: i64,
}

impl CapturedStat {
    #[cfg(test)]
    pub(crate) fn from_metadata(meta: &std::fs::Metadata) -> Self {
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

    pub(crate) fn from_raw_lstat(raw: &crate::utils::beneath::RawLstat) -> Self {
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

/// One `read_dir` name plus the worker-side `file_type()` (d_type / lstat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Dirent {
    pub(crate) name: Vec<u8>,
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) is_file: bool,
    pub(crate) type_ok: bool,
}

impl Dirent {
    #[cfg(test)]
    pub(crate) fn from_dir_entry(entry: &std::fs::DirEntry) -> Self {
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

    pub(crate) fn from_fd_dirent(entry: &crate::utils::beneath::FdDirent) -> Self {
        let name = path_to_bytes(&PathBuf::from(&entry.name));
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
    pub(crate) is_dir: bool,
    pub(crate) is_file: bool,
    pub(crate) is_symlink: bool,
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
    pub(crate) entries: Vec<Dirent>,
    pub(crate) error_kinds: Vec<(u8, Option<i32>)>,
    pub(crate) taken: usize,
    pub(crate) hit_cap: bool,
    #[serde(default)]
    pub(crate) timed_out: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CapRequest {
    pub(crate) cap: String,
    pub(crate) request: IoRequest,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum IoRequest {
    SymlinkMetadata {
        /// A canonical relative path beneath `root`; empty means `root`.
        path: Vec<u8>,
        /// An absolute, sealed worktree root.
        root: Vec<u8>,
    },
    CanonicalizePair {
        /// Canonical relative paths beneath `root`; empty means `root`.
        left: Vec<u8>,
        right: Vec<u8>,
        /// An absolute, sealed worktree root.
        root: Vec<u8>,
    },
    ReadDir {
        /// A canonical relative path beneath `root`; empty means `root`.
        path: Vec<u8>,
        /// An absolute, sealed worktree root.
        root: Vec<u8>,
        remaining: usize,
        checkpoint_every: u32,
    },
    FileBlobHash {
        /// A canonical relative path beneath `root`.
        path: Vec<u8>,
        /// An absolute, sealed worktree root.
        root: Vec<u8>,
        hash_kind: String,
        /// Parent invocation nonce. A non-zero value bounds helper-side
        /// attribute-source negative caching to one status invocation.
        #[serde(default)]
        root_session: u64,
    },
    ReadObjectBlob {
        oid: String,
        objects_root: Vec<u8>,
        byte_limit: u64,
        hash_kind: String,
    },
    MarkerProbe {
        /// A canonical relative path beneath `root`; empty means `root`.
        dir: Vec<u8>,
        /// An absolute, sealed worktree root.
        root: Vec<u8>,
    },
    Shutdown,
}

impl IoRequest {
    /// Validate an incoming request before it is serialized or dispatched.
    ///
    /// This is deliberately lexical only: validating a request must not probe
    /// the filesystem because it runs once in the parent and again after wire
    /// decoding in the helper. The handler seals the root once, then every
    /// read uses the beneath no-follow operations for TOCTOU enforcement.
    pub(crate) fn validate(&self) -> io::Result<()> {
        match self {
            Self::SymlinkMetadata { path, root } | Self::ReadDir { path, root, .. } => {
                validate_worktree_path(root, path, true).map(|_| ())
            }
            Self::CanonicalizePair { left, right, root } => {
                validate_worktree_path(root, left, true)?;
                validate_worktree_path(root, right, true).map(|_| ())
            }
            Self::FileBlobHash { path, root, .. } => {
                validate_worktree_path(root, path, false).map(|_| ())
            }
            Self::MarkerProbe { dir, root } => validate_worktree_path(root, dir, true).map(|_| ()),
            Self::ReadObjectBlob {
                oid, objects_root, ..
            } => {
                validate_absolute_root(objects_root, "object-store")?;
                if !matches!(oid.len(), 40 | 64)
                    || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "object-store request must contain a 40- or 64-digit hexadecimal OID",
                    ));
                }
                Ok(())
            }
            Self::Shutdown => Ok(()),
        }
    }
}

fn validate_absolute_root(bytes: &[u8], kind: &str) -> io::Result<()> {
    let root = bytes_to_path(bytes);
    if !root.is_absolute() || root.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{kind} capability root must be an absolute path"),
        ));
    }
    validate_absolute_root_units(bytes, kind)?;
    if root
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{kind} capability root must be lexically canonical"),
        ));
    }
    Ok(())
}

fn validate_absolute_root_units(bytes: &[u8], kind: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        let has_dot_segment = bytes
            .split(|byte| *byte == b'/')
            .any(|segment| segment == b"." || segment == b"..");
        if has_dot_segment {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{kind} capability root must be lexically canonical"),
            ));
        }
        if bytes.windows(2).any(|pair| pair == b"//") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{kind} capability root must not contain duplicate separators"),
            ));
        }
    }
    #[cfg(windows)]
    {
        if bytes.len() % 2 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{kind} capability root has invalid platform path encoding"),
            ));
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        let is_separator = |unit: u16| unit == u16::from(b'\\') || unit == u16::from(b'/');
        let allows_unc_prefix =
            units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]);
        let is_dot_segment = |segment: &[u16]| {
            (segment.len() == 1 && segment[0] == u16::from(b'.'))
                || (segment.len() == 2
                    && segment[0] == u16::from(b'.')
                    && segment[1] == u16::from(b'.'))
        };
        let mut segment_start = 0;
        let mut previous_separator = false;
        for (index, unit) in units.iter().copied().enumerate() {
            if !is_separator(unit) {
                previous_separator = false;
                continue;
            }
            if previous_separator && !(allows_unc_prefix && index == 1) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{kind} capability root must not contain duplicate separators"),
                ));
            }
            if is_dot_segment(&units[segment_start..index]) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{kind} capability root must be lexically canonical"),
                ));
            }
            segment_start = index + 1;
            previous_separator = true;
        }
        if is_dot_segment(&units[segment_start..]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{kind} capability root must be lexically canonical"),
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (bytes, kind);
    }
    Ok(())
}

fn validate_worktree_path(
    root_bytes: &[u8],
    path_bytes: &[u8],
    allow_root: bool,
) -> io::Result<PathBuf> {
    validate_absolute_root(root_bytes, "worktree")?;
    let path = bytes_to_path(path_bytes);
    if allow_root {
        if path.as_os_str().is_empty() {
            return Ok(PathBuf::new());
        }
        validate_relative_path(&path)?;
    } else {
        validate_relative_path(&path)?;
    }
    Ok(path)
}

/// Convert a status caller's absolute or relative path into the strict
/// relative representation carried on the wire without probing the root.
/// The helper process seals the root before it performs any I/O.
pub(crate) fn relative_worktree_path(
    root_bytes: &[u8],
    path: &Path,
    allow_root: bool,
) -> io::Result<PathBuf> {
    validate_absolute_root(root_bytes, "worktree")?;
    let root = bytes_to_path(root_bytes);
    let relative = if path.is_absolute() {
        path.strip_prefix(&root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree request path is outside its root",
            )
        })?
    } else {
        path
    };
    if relative.as_os_str().is_empty() {
        if allow_root {
            return Ok(PathBuf::new());
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree request path must be a non-empty canonical relative path",
        ));
    }
    validate_relative_path(relative)?;
    Ok(relative.to_path_buf())
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum IoEvent {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObjectBlobStatus {
    Ok,
    Missing,
    Corrupt,
    Unavailable,
    TooLarge,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WireResult<T> {
    Ok(T),
    Err { kind: u8, raw_os: Option<i32> },
}

pub(crate) fn kind_to_u8(kind: io::ErrorKind) -> u8 {
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

pub(crate) fn wire_result<T>(result: io::Result<T>) -> WireResult<T> {
    match result {
        Ok(value) => WireResult::Ok(value),
        Err(error) => WireResult::Err {
            kind: kind_to_u8(error.kind()),
            raw_os: error.raw_os_error(),
        },
    }
}

pub(crate) fn unwrap_wire<T>(result: WireResult<T>) -> io::Result<T> {
    match result {
        WireResult::Ok(value) => Ok(value),
        WireResult::Err { kind, raw_os } => Err(io_from_wire(kind, raw_os)),
    }
}

pub(crate) fn path_to_bytes(path: &Path) -> Vec<u8> {
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

pub(crate) fn bytes_to_path(bytes: &[u8]) -> PathBuf {
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

pub(crate) fn write_frame(writer: &mut impl Write, event: &IoEvent) -> io::Result<()> {
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

pub(crate) fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> io::Result<T> {
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

pub(crate) fn write_raw_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
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

pub(crate) fn parse_event_frames(mut data: &[u8]) -> Option<Vec<IoEvent>> {
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

pub(crate) fn write_request(
    writer: &mut impl Write,
    token: &str,
    request: IoRequest,
) -> io::Result<()> {
    request.validate()?;
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

#[cfg(test)]
mod tests {
    use std::{io, path::Path};

    use tempfile::tempdir;

    use super::{
        FRAME_CAP, IoEvent, IoRequest, ObjectBlobStatus, ObjectStoreCapability, WireResult,
        WorktreeRootCapability, io_from_wire, parse_event_frames, path_to_bytes, unwrap_wire,
        wire_result, write_frame, write_raw_frame, write_request,
    };

    #[test]
    fn worktree_capability_accepts_only_canonical_relative_paths() {
        let root = tempdir().expect("create worktree root");
        let capability = WorktreeRootCapability::seal(root.path()).expect("seal root");
        assert_eq!(
            capability
                .relative(Path::new("src/main.rs"))
                .expect("normal path"),
            Path::new("src/main.rs")
        );
        for invalid in [
            Path::new(""),
            Path::new("."),
            Path::new("./main.rs"),
            Path::new("src/../main.rs"),
            Path::new("src//main.rs"),
            Path::new("/tmp/escape"),
        ] {
            assert!(
                capability.relative(invalid).is_err(),
                "path must be rejected: {invalid:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn request_validation_accepts_windows_absolute_roots_only() {
        for root in [Path::new("C:\\"), Path::new(r"\\server\share")] {
            let request = IoRequest::MarkerProbe {
                dir: b"nested".to_vec(),
                root: path_to_bytes(root),
            };
            assert!(
                request.validate().is_ok(),
                "absolute Windows root must be lexically valid: {root:?}"
            );
        }

        for root in [
            Path::new(r"C:\worktree\..\escape"),
            Path::new(r"C:\worktree\.\nested"),
            Path::new(r"C:\worktree\\nested"),
            Path::new(r"\\server\share\\nested"),
            Path::new(r"worktree"),
        ] {
            let request = IoRequest::MarkerProbe {
                dir: b"nested".to_vec(),
                root: path_to_bytes(root),
            };
            assert!(
                request.validate().is_err(),
                "non-canonical or relative Windows root must fail: {root:?}"
            );
        }

        let request = IoRequest::MarkerProbe {
            dir: path_to_bytes(Path::new(r"C:\escape")),
            root: path_to_bytes(Path::new(r"C:\worktree")),
        };
        assert!(
            request.validate().is_err(),
            "absolute request path must remain rejected"
        );
    }

    #[test]
    fn request_validation_is_lexical_and_does_not_probe_root() {
        let root = tempdir()
            .expect("create temporary parent")
            .path()
            .to_path_buf();
        let missing_root = root.join("missing-worktree-root");
        let request = IoRequest::MarkerProbe {
            dir: b".libra".to_vec(),
            root: path_to_bytes(&missing_root),
        };

        // The root has been removed with the temporary parent, but lexical
        // validation is still expected to pass. The helper seals it later.
        assert!(request.validate().is_ok());
        let mut wire = Vec::new();
        write_request(&mut wire, "test-cap", request).expect("lexically valid request encodes");
        assert!(!wire.is_empty());
    }

    #[test]
    fn object_store_and_worktree_capabilities_are_separate_types() {
        let worktree = tempdir().expect("create worktree root");
        let objects = tempdir().expect("create object root");
        let worktree_capability = WorktreeRootCapability::seal(worktree.path()).expect("seal");
        let object_capability = ObjectStoreCapability::seal(objects.path()).expect("seal");

        fn accept_worktree(_: WorktreeRootCapability) {}
        fn accept_object_store(_: ObjectStoreCapability) {}
        accept_worktree(worktree_capability.clone());
        accept_object_store(object_capability.clone());
        assert!(
            object_capability
                .object_path(&"a".repeat(40))
                .expect("valid OID")
                .starts_with(object_capability.root())
        );
        assert!(object_capability.object_path("../escape").is_err());
        assert!(
            worktree_capability
                .relative(Path::new("../escape"))
                .is_err()
        );
    }

    #[test]
    fn oversized_frames_and_blobs_are_rejected_before_output() {
        let mut json_wire = Vec::new();
        let json_error = write_frame(
            &mut json_wire,
            &IoEvent::Error {
                message: "x".repeat(FRAME_CAP),
            },
        )
        .expect_err("JSON frame must be capped");
        assert_eq!(json_error.kind(), io::ErrorKind::InvalidData);
        assert!(json_wire.is_empty());

        let mut raw_wire = Vec::new();
        let raw_error = write_raw_frame(&mut raw_wire, &vec![0u8; FRAME_CAP + 1])
            .expect_err("raw frame must be capped");
        assert_eq!(raw_error.kind(), io::ErrorKind::InvalidData);
        assert!(raw_wire.is_empty());

        let mut object_wire = Vec::new();
        write_object_blob_outcome_for_test(&mut object_wire);
        let events = parse_event_frames(&object_wire).expect("TooLarge event remains framed");
        assert!(matches!(
            events.as_slice(),
            [IoEvent::DoneObjectBlob {
                status: ObjectBlobStatus::TooLarge,
                bytes: None,
            }]
        ));
    }

    #[test]
    fn wire_errors_preserve_kind_and_raw_os_error() {
        let encoded = wire_result::<()>(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied",
        )));
        let WireResult::Err { kind, raw_os } = &encoded else {
            panic!("permission error must be encoded");
        };
        assert_eq!(*kind, 1);
        assert_eq!(*raw_os, None);
        let decoded = unwrap_wire(encoded).expect_err("wire error must decode");
        assert_eq!(decoded.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(decoded.raw_os_error(), None);

        let encoded = wire_result::<()>(Err(io::Error::from_raw_os_error(2)));
        let WireResult::Err { raw_os, .. } = &encoded else {
            panic!("raw OS error must be encoded");
        };
        assert_eq!(*raw_os, Some(2));
        let decoded = unwrap_wire(encoded).expect_err("wire error must decode");
        assert_eq!(decoded.raw_os_error(), Some(2));
        assert_eq!(
            io_from_wire(1, None).kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn request_encoder_rejects_non_relative_worktree_paths() {
        let root = tempdir().expect("create worktree root");
        let mut wire = Vec::new();
        let request = IoRequest::ReadDir {
            path: path_to_bytes(Path::new("../escape")),
            root: path_to_bytes(root.path()),
            remaining: 1,
            checkpoint_every: 1,
        };

        let error = write_request(&mut wire, "token", request)
            .expect_err("request encoder must reject path traversal");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(wire.is_empty(), "invalid requests must not be serialized");
    }

    // Kept local to protocol tests so the blob status ordering contract is
    // exercised without exposing the command worker's object-store handler.
    fn write_object_blob_outcome_for_test(writer: &mut Vec<u8>) {
        write_frame(
            writer,
            &IoEvent::DoneObjectBlob {
                status: ObjectBlobStatus::TooLarge,
                bytes: None,
            },
        )
        .expect("TooLarge event fits in one frame");
    }
}
