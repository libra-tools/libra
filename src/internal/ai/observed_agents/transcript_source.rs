//! DR-04a — the unified `TranscriptSource` seam (ADR-DR-02).
//!
//! This is the **single writer read entry point** for external-agent
//! transcript content. Both the live checkpoint writer
//! (`capture::live_checkpoint::write_committed_checkpoint`) and the import
//! writer resolve their bytes through [`resolve_transcript_source`], never by
//! re-opening a path themselves.
//!
//! Two source shapes exist (ADR-DR-02):
//!
//! - [`TranscriptSource::File`] — a provider-root-authorized, already-opened
//!   file handle ([`AuthorizedTranscriptFile`]). The handle is opened **once**
//!   inside the resolver after the provider-root precheck; the writer reads
//!   from the open descriptor and must never re-open by path, so a
//!   post-authorization path swap (symlink flip / TOCTOU) cannot change the
//!   bytes it reads.
//! - [`TranscriptSource::Bytes`] — in-memory bytes carrying an
//!   [`ExportAuthorized`] tag. This shape is constructed only by a trusted
//!   in-process producer: the OpenCode export bridge (DR-04b), or a securely
//!   discovered Claude child source that is being handed to the capture
//!   snapshot boundary. There is no public way to forge the tag, so the
//!   writer will not treat an arbitrary `&[u8]` as a trusted source.
//!
//! Security note (ADR-DR-13): containment is enforced by the current pinned,
//! descriptor-relative safe-open implementation. On Unix it walks the
//! provider root and source with `openat(O_NOFOLLOW)` rather than trusting a
//! deferred path check; unsupported platforms fail closed. The writer then
//! consumes only that held descriptor and never re-opens the source path.

#[cfg(unix)]
use std::path::Component;
use std::{
    fmt,
    io::Seek,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use thiserror::Error;

use crate::internal::ai::{
    authorized_read::{StrictBoundedRead, read_strictly_bounded},
    observed_agents::{AgentSessionCtx, ObservedAgent},
};

/// Default effective byte cap for a single transcript read (GC-DR-04). Matches
/// the existing Claude adapter hard cap so DR-04a does not silently enlarge the
/// hook-path memory ceiling.
pub const TRANSCRIPT_READ_HARD_CAP_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum TranscriptReadError {
    #[error("transcript exceeds {cap} byte cap; refusing to load")]
    ExceedsCap { cap: u64 },
}

/// Proof token that a [`TranscriptSource::File`] was opened from inside the
/// provider's own transcript root. Its field is private, so it can only be
/// minted by [`resolve_transcript_source`] in this module.
#[derive(Debug)]
pub struct ProviderRootAuthorized(());

/// The trusted in-process producer that authorized an in-memory transcript.
///
/// This is crate-scoped deliberately: the wire-facing snapshot projection
/// retains only its safe classification, never this proof object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InMemoryTranscriptOrigin {
    TrustedExport,
    DiscoveredSubagent,
}

/// Proof token that a [`TranscriptSource::Bytes`] payload came from a trusted
/// in-process producer. Fields are private and constructors bind the tag to
/// the exact bytes via SHA-256 — so no caller outside this crate can mint a
/// tag, and a tag cannot be re-attached to different bytes: the writer
/// re-verifies with [`ExportAuthorized::matches`].
#[derive(Clone)]
pub struct ExportAuthorized {
    agent_kind: String,
    session_id: String,
    content_digest: String,
    origin: InMemoryTranscriptOrigin,
}

impl std::fmt::Debug for ExportAuthorized {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExportAuthorized")
            .field("origin", &self.origin)
            .field("sensitive_fields", &"<redacted>")
            .finish()
    }
}

impl ExportAuthorized {
    /// Mint an authorization tag for freshly exported `bytes`. Crate-scoped:
    /// only the verified export bridge (DR-04b) may issue this export form.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn issue(agent_kind: &str, session_id: &str, bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            agent_kind: agent_kind.to_string(),
            session_id: session_id.to_string(),
            content_digest: hex::encode(Sha256::digest(bytes)),
            origin: InMemoryTranscriptOrigin::TrustedExport,
        }
    }

    /// Mint the distinct proof form used only after Claude child discovery
    /// has securely read a provider-root source. The provider-relative source
    /// key remains in discovery's transient ownership/linking state; it is
    /// intentionally not copied into this authorization token or any durable
    /// snapshot projection, even as an unkeyed digest.
    pub(crate) fn issue_discovered_subagent(
        agent_kind: &str,
        session_id: &str,
        bytes: &[u8],
    ) -> Self {
        use sha2::{Digest, Sha256};

        Self {
            agent_kind: agent_kind.to_string(),
            session_id: session_id.to_string(),
            content_digest: hex::encode(Sha256::digest(bytes)),
            origin: InMemoryTranscriptOrigin::DiscoveredSubagent,
        }
    }

    /// Verify the tag is bound to this session AND to these exact bytes
    /// (recomputes the SHA-256). The writer must reject the source when this
    /// returns false.
    pub fn matches(&self, agent_kind: &str, session_id: &str, bytes: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        self.agent_kind == agent_kind
            && self.session_id == session_id
            && self.content_digest == hex::encode(Sha256::digest(bytes))
    }

    pub fn agent_kind(&self) -> &str {
        &self.agent_kind
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn origin(&self) -> InMemoryTranscriptOrigin {
        self.origin
    }
}

/// A transcript file that has already been safely opened inside the provider
/// root. The writer reads from the held descriptor; the path is retained only
/// for diagnostics / `source_id` derivation and is **never** re-opened.
pub struct AuthorizedTranscriptFile {
    file: std::fs::File,
}

impl std::fmt::Debug for AuthorizedTranscriptFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedTranscriptFile")
            .field("file", &"<descriptor>")
            .finish()
    }
}

impl AuthorizedTranscriptFile {
    pub(crate) fn into_rewound_inner(mut self) -> Result<std::fs::File> {
        self.file
            .seek(std::io::SeekFrom::Start(0))
            .context("rewind authorized transcript before reader handoff")?;
        Ok(self.file)
    }

    /// Read the transcript from the already-open descriptor, refusing to load
    /// anything larger than `cap` (matching the existing adapter baseline,
    /// which errors on oversize rather than silently truncating). Reads never
    /// re-open by path, so a concurrent path swap cannot change the bytes.
    pub fn read_bounded(&mut self, cap: u64) -> Result<Vec<u8>> {
        self.read_bounded_counted(cap).0
    }

    /// Count every byte pulled from the held descriptor even when the read is
    /// rejected (for example, the `cap + 1` oversize sentinel). Historical
    /// batch import uses this to enforce its cumulative budget across failed
    /// as well as successfully parsed candidates.
    pub(crate) fn read_bounded_counted(&mut self, cap: u64) -> (Result<Vec<u8>>, u64) {
        if let Err(error) = self
            .file
            .seek(std::io::SeekFrom::Start(0))
            .context("rewind authorized transcript before read")
        {
            return (Err(error), 0);
        }
        match read_strictly_bounded(&mut self.file, cap) {
            StrictBoundedRead::Complete(bytes) => {
                let bytes_read = bytes.len() as u64;
                (Ok(bytes), bytes_read)
            }
            StrictBoundedRead::Oversize { observed_bytes } => (
                Err(TranscriptReadError::ExceedsCap { cap }.into()),
                observed_bytes,
            ),
            StrictBoundedRead::Failed { bytes_read, error } => (
                Err(error).context("read authorized transcript handle"),
                bytes_read,
            ),
        }
    }

    fn len(&self) -> Result<u64> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .context("inspect authorized transcript size")
    }

    fn descriptor(&self) -> &std::fs::File {
        &self.file
    }

    /// Read a bounded preview and rewind the already-authorized descriptor.
    /// Used only after import consent to derive a provider session id for an
    /// explicit `--path`; the subsequent writer still consumes this exact
    /// held handle rather than reopening the path.
    pub fn preview_bounded(&mut self, cap: u64) -> Result<Vec<u8>> {
        let start = self
            .file
            .stream_position()
            .context("read authorized transcript position")?;
        let bytes = self.read_bounded(cap)?;
        self.file
            .seek(std::io::SeekFrom::Start(start))
            .context("rewind authorized transcript handle after preview")?;
        Ok(bytes)
    }
}

/// The unified writer read source (ADR-DR-02).
pub enum TranscriptSource {
    File {
        file: AuthorizedTranscriptFile,
        /// Provider-root-relative source identity (never an absolute home
        /// path — GC-DR-13 / ADR-DR-08 #6).
        source_id: String,
        auth: ProviderRootAuthorized,
    },
    Bytes {
        bytes: Vec<u8>,
        auth: ExportAuthorized,
    },
}

/// Source debugging is intentionally content-free.  A trusted export still
/// contains native/raw transcript bytes at this boundary; deriving `Debug`
/// for the `Vec<u8>` would leak them through otherwise harmless diagnostics.
impl fmt::Debug for TranscriptSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File { source_id, .. } => formatter
                .debug_struct("TranscriptSource::File")
                .field("source_id_len", &source_id.len())
                .finish(),
            Self::Bytes { bytes, .. } => formatter
                .debug_struct("TranscriptSource::Bytes")
                .field("byte_len", &bytes.len())
                .finish(),
        }
    }
}

/// Classified result of resolving an external-agent transcript source.
///
/// The legacy [`resolve_transcript_source`] API intentionally folds a missing
/// locator and a rejected locator into `Ok(None)`: its only consumer used to
/// need a prompt fallback in either case. Capture snapshots need to preserve
/// that security distinction in safe metadata, however. This enum carries no
/// path or error text, so it is safe to turn into a durable partial reason.
pub enum TranscriptSourceResolution {
    /// A descriptor-pinned file or export-backed byte source was authorized.
    Authorized(TranscriptSource),
    /// No source was supplied, or the supplied source disappeared before it
    /// could be opened.
    Absent,
    /// A caller supplied a source outside the adapter's protected provider
    /// root. Its path is intentionally not retained here.
    Untrusted,
}

impl fmt::Debug for TranscriptSourceResolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authorized(source) => formatter
                .debug_tuple("TranscriptSourceResolution::Authorized")
                .field(source)
                .finish(),
            Self::Absent => formatter.write_str("TranscriptSourceResolution::Absent"),
            Self::Untrusted => formatter.write_str("TranscriptSourceResolution::Untrusted"),
        }
    }
}

impl TranscriptSource {
    /// Authorized raw size used by the command's cumulative batch budget.
    /// This inspects the held descriptor or in-memory export; it never
    /// reopens a provider path.
    pub fn authorized_len(&self) -> Result<u64> {
        match self {
            Self::File { file, .. } => file.len(),
            Self::Bytes { bytes, .. } => {
                u64::try_from(bytes.len()).context("export byte length exceeds u64")
            }
        }
    }
}

/// Normalize the fixed macOS system aliases before descriptor-relative
/// no-follow traversal. These aliases are OS-owned, unlike any component below
/// them, which must remain subject to the strict symlink checks.
fn normalize_macos_system_directory_alias(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
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
    #[cfg(not(target_os = "macos"))]
    {
        path.to_path_buf()
    }
}

fn provider_root_containing(adapter: &dyn ObservedAgent, canonical_path: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    adapter.protected_dirs().iter().find_map(|dir| {
        let root = if *dir == ".codex" {
            match std::env::var_os("CODEX_HOME").map(PathBuf::from) {
                Some(path) if path.is_absolute() => path,
                _ => home.join(dir),
            }
        } else {
            home.join(dir)
        };
        let root = normalize_macos_system_directory_alias(&root)
            .canonicalize()
            .ok()?;
        canonical_path.starts_with(&root).then_some(root)
    })
}

fn configured_provider_roots(adapter: &dyn ObservedAgent) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
    else {
        return Vec::new();
    };
    adapter
        .protected_dirs()
        .iter()
        .map(|dir| {
            let root = if *dir == ".codex" {
                match std::env::var_os("CODEX_HOME").map(PathBuf::from) {
                    Some(path) if path.is_absolute() => path,
                    _ => home.join(dir),
                }
            } else {
                home.join(dir)
            };
            normalize_macos_system_directory_alias(&root)
        })
        .collect()
}

/// Open a provider transcript relative to a pinned provider root while
/// rejecting every symlink/magic component. Unix uses descriptor-relative
/// `openat(O_NOFOLLOW)` for each component; platforms without equivalent
/// semantics fail closed (ADR-DR-13/GC-DR-14).
#[cfg(unix)]
fn open_absolute_directory_no_follow(path: &Path) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    if !path.is_absolute() {
        anyhow::bail!("provider root must be absolute");
    }
    let slash = CString::new("/").context("construct root directory name")?;
    // SAFETY: slash is NUL-terminated and a successful descriptor is owned
    // immediately below.
    let root_fd = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open filesystem root");
    }
    // SAFETY: `root_fd` is a fresh descriptor returned by `open`.
    let mut current = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(root_fd) };
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = CString::new(name.as_bytes())
                    .context("provider root component contains NUL")?;
                // SAFETY: `current` is a live directory descriptor and name
                // is NUL-terminated. A successful fd is owned immediately.
                let fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("securely open provider root component (no-follow)");
                }
                // SAFETY: `fd` is a fresh descriptor returned by `openat`.
                current = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
            }
            _ => anyhow::bail!("provider root contains a non-normal component"),
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_beneath_no_follow(root: &Path, relative: &Path) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    let mut current = open_absolute_directory_no_follow(root)
        .context("securely open provider transcript root (no-follow)")?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        anyhow::bail!("provider transcript path does not name a file");
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("provider transcript path contains a non-normal component");
        };
        let name = CString::new(name.as_bytes())
            .context("provider transcript path component contains NUL")?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                // Prevent a FIFO/device candidate from blocking before the
                // descriptor's file type can be checked.
                libc::O_RDONLY | libc::O_NONBLOCK
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY
            };
        // SAFETY: `current` owns a live directory fd, `name` is NUL-terminated,
        // and a successful return is immediately wrapped in an owned File.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("securely open provider transcript component (no-follow)");
        }
        // SAFETY: `fd` is a fresh descriptor returned by openat above.
        let opened = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let meta = opened
            .metadata()
            .context("inspect securely opened provider transcript component")?;
        if final_component {
            if !meta.is_file() {
                anyhow::bail!("provider transcript source is not a regular file");
            }
            return Ok(opened);
        }
        if !meta.is_dir() {
            anyhow::bail!("provider transcript path component is not a directory");
        }
        current = opened;
    }
    anyhow::bail!("provider transcript path did not resolve to a file")
}

#[cfg(not(unix))]
fn open_beneath_no_follow(_root: &Path, _relative: &Path) -> Result<std::fs::File> {
    anyhow::bail!(
        "secure provider transcript opening is unavailable on this platform; import fails closed"
    )
}

/// Open a provider-owned directory for pre-consent discovery without ever
/// following a symlinked component. The returned descriptor pins the
/// directory while callers enumerate it.
#[cfg(unix)]
pub(crate) fn open_provider_directory_for_discovery(
    adapter: &dyn ObservedAgent,
    path: &Path,
) -> Result<Option<std::fs::File>> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    if !path.is_absolute() {
        return Ok(None);
    }
    // Mirror `securely_open_provider_file`: the roots below come back through
    // `normalize_macos_system_directory_alias`, so the query path must be
    // normalized the same way or `strip_prefix` never matches on macOS (for
    // example, `/tmp` is a symlink to `/private/tmp`) — and a silent `Ok(None)`
    // here means the no-follow / budget hardening below never runs at all.
    let path = normalize_macos_system_directory_alias(path);
    for root in configured_provider_roots(adapter) {
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let mut current = match open_absolute_directory_no_follow(&root) {
            Ok(directory) => directory,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).context("securely open provider discovery root (no-follow)");
            }
        };
        for component in relative.components() {
            let Component::Normal(name) = component else {
                anyhow::bail!("provider discovery directory contains a non-normal component");
            };
            let name = CString::new(name.as_bytes())
                .context("provider discovery directory component contains NUL")?;
            // SAFETY: `current` owns a live directory fd and `name` is
            // NUL-terminated. A successful fd is immediately owned.
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(error)
                    .context("securely open provider discovery component (no-follow)");
            }
            // SAFETY: `fd` is a fresh descriptor returned by `openat`.
            current = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        }
        return Ok(Some(current));
    }
    Ok(None)
}

/// File type of one entry yielded by [`read_dir_pinned_provider_directory`],
/// resolved relative to the pinned directory descriptor so it always refers
/// to the opened directory object even when its pathname was swapped.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinnedEntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[cfg(unix)]
impl PinnedEntryType {
    pub(crate) fn is_file(self) -> bool {
        matches!(self, Self::File)
    }

    pub(crate) fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// One entry of a pinned provider directory: its name plus a
/// descriptor-relative file type.
#[cfg(unix)]
pub(crate) struct PinnedDirEntry {
    pub(crate) file_name: std::ffi::OsString,
    pub(crate) file_type: PinnedEntryType,
}

/// Streaming iterator over a pinned provider directory, mirroring
/// `std::fs::ReadDir` semantics: entries arrive in `readdir` order (unsorted,
/// `.` and `..` skipped) and a mid-walk read failure surfaces as an `Err`
/// item. Owns the `DIR*` — and with it the duplicated descriptor — so every
/// exit path, including error paths, releases both exactly once.
#[cfg(unix)]
pub(crate) struct PinnedReadDir {
    stream: *mut libc::DIR,
}

#[cfg(unix)]
impl Iterator for PinnedReadDir {
    type Item = Result<PinnedDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        use std::{
            ffi::{CStr, OsStr},
            os::unix::ffi::OsStrExt,
        };

        loop {
            clear_errno();
            // SAFETY: `self.stream` is a live directory stream owned by
            // `self`; the returned pointer stays valid until the next
            // `readdir`/`closedir` on this stream, and every field is copied
            // out before the next call.
            let entry = unsafe { libc::readdir(self.stream) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                return match error.raw_os_error() {
                    // errno was cleared above: still zero means a clean EOF.
                    Some(0) | None => None,
                    _ => Some(Err(error).context("read pinned provider directory entry")),
                };
            }
            // SAFETY: `entry` is non-null and points to a live `dirent`.
            let entry = unsafe { &*entry };
            // SAFETY: `d_name` is NUL-terminated within the live `dirent`.
            let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
            let name_bytes = name.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            let file_type = match pinned_entry_type(self.stream, name, entry.d_type.into()) {
                Ok(Some(file_type)) => file_type,
                // Vanished between `readdir` and the DT_UNKNOWN fallback.
                Ok(None) => continue,
                Err(error) => return Some(Err(error)),
            };
            return Some(Ok(PinnedDirEntry {
                file_name: OsStr::from_bytes(name_bytes).to_os_string(),
                file_type,
            }));
        }
    }
}

#[cfg(unix)]
impl Drop for PinnedReadDir {
    fn drop(&mut self) {
        // SAFETY: `self.stream` is a live stream from `fdopendir`, closed
        // exactly once here; closing also releases the duplicated descriptor.
        unsafe { libc::closedir(self.stream) };
    }
}

/// Zero `errno` so a `readdir` EOF (NULL with `errno` unchanged) can be told
/// apart from a real read error.
#[cfg(unix)]
fn clear_errno() {
    #[cfg(target_os = "linux")]
    // SAFETY: `__errno_location` returns the writable thread-local slot.
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    // SAFETY: `__error` returns the writable thread-local slot.
    unsafe {
        *libc::__error() = 0;
    }
}

/// Map `readdir`'s `d_type` to a [`PinnedEntryType`].  A filesystem that
/// reports `DT_UNKNOWN` is resolved via a descriptor-relative
/// `fstatat(AT_SYMLINK_NOFOLLOW)` — never by path, never by guessing.  An
/// entry that vanishes between the two calls yields `Ok(None)`; any other
/// inspection failure is loud.
#[cfg(unix)]
fn pinned_entry_type(
    stream: *mut libc::DIR,
    name: &std::ffi::CStr,
    d_type: libc::c_int,
) -> Result<Option<PinnedEntryType>> {
    use std::mem::MaybeUninit;

    if d_type == libc::DT_UNKNOWN as libc::c_int {
        // SAFETY: `stream` is a live directory stream, so `dirfd` yields the
        // pinned descriptor it owns.
        let fd = unsafe { libc::dirfd(stream) };
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fd` is the live pinned directory descriptor and `name` is
        // a NUL-terminated basename from the live dirent; AT_SYMLINK_NOFOLLOW
        // makes the result describe the entry itself, not a link target.
        let result = unsafe {
            libc::fstatat(
                fd,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).context("inspect pinned provider directory entry type");
        }
        // SAFETY: a successful fstatat initialized the stat structure.
        let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
        return Ok(Some(if mode == libc::S_IFREG {
            PinnedEntryType::File
        } else if mode == libc::S_IFDIR {
            PinnedEntryType::Directory
        } else if mode == libc::S_IFLNK {
            PinnedEntryType::Symlink
        } else {
            PinnedEntryType::Other
        }));
    }
    Ok(Some(if d_type == libc::DT_REG as libc::c_int {
        PinnedEntryType::File
    } else if d_type == libc::DT_DIR as libc::c_int {
        PinnedEntryType::Directory
    } else if d_type == libc::DT_LNK as libc::c_int {
        PinnedEntryType::Symlink
    } else {
        PinnedEntryType::Other
    }))
}

/// Enumerate a held provider directory descriptor without ever re-resolving
/// its pathname.  This replaces addressing the descriptor through a path
/// (`/proc/self/fd/<fd>` on Linux, `/dev/fd/<fd>` elsewhere): on macOS a
/// device-fd path for a directory descriptor fails `read_dir` with
/// `ENOTDIR`.  `fdopendir` + `readdir` operate on a duplicated descriptor
/// instead, so the walk is rooted at the exact directory object opened by
/// [`open_provider_directory_for_discovery`] even if an attacker swaps the
/// provider's pathname concurrently — with identical behaviour on Linux and
/// macOS and no per-OS fork.
#[cfg(unix)]
pub(crate) fn read_dir_pinned_provider_directory(
    directory: &std::fs::File,
) -> Result<PinnedReadDir> {
    use std::os::fd::AsRawFd;

    // SAFETY: duplicating a live owned descriptor yields another owned
    // descriptor referring to the same pinned directory object.
    let duplicated = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error())
            .context("duplicate pinned provider directory descriptor");
    }
    // SAFETY: `duplicated` is a fresh descriptor from `dup`; on success
    // `fdopendir` takes ownership of it.
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `duplicated` is still owned here
        // and must be closed exactly once.
        unsafe { libc::close(duplicated) };
        return Err(error).context("open pinned provider directory stream");
    }
    Ok(PinnedReadDir { stream })
}

/// Open a regular file beneath an already-pinned provider directory without
/// following any descendant symlink.  Callers keep the directory descriptor
/// alive across enumeration and pass the provider-relative entry back here,
/// closing the usual `read_dir` check-to-open race.
#[cfg(unix)]
pub(crate) fn open_file_beneath_pinned_provider_directory(
    directory: &std::fs::File,
    relative: &Path,
) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    // SAFETY: duplicating a live owned descriptor yields another owned
    // descriptor referring to the same pinned directory object.
    let duplicated = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error())
            .context("duplicate pinned provider directory descriptor");
    }
    // SAFETY: `duplicated` is a fresh descriptor returned by `dup`.
    let mut current = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(duplicated) };
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        anyhow::bail!("provider-relative source does not name a file");
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("provider-relative source contains a non-normal component");
        };
        let name = CString::new(name.as_bytes())
            .context("provider-relative source component contains NUL")?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: `current` owns a live directory fd and `name` is a valid
        // NUL-terminated component. A successful fd is immediately owned.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("securely open pinned provider descendant (no-follow)");
        }
        // SAFETY: `fd` is a fresh descriptor returned by `openat`.
        let opened = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let metadata = opened
            .metadata()
            .context("inspect pinned provider descendant")?;
        if final_component {
            if !metadata.is_file() {
                anyhow::bail!("provider descendant source is not a regular file");
            }
            return Ok(opened);
        }
        if !metadata.is_dir() {
            anyhow::bail!("provider descendant component is not a directory");
        }
        current = opened;
    }
    anyhow::bail!("provider-relative source did not resolve to a file")
}

#[cfg(not(unix))]
pub(crate) fn open_file_beneath_pinned_provider_directory(
    _directory: &std::fs::File,
    _relative: &Path,
) -> Result<std::fs::File> {
    anyhow::bail!(
        "secure pinned provider descendant opening is unavailable on this platform; capture fails closed"
    )
}

#[cfg(not(unix))]
pub(crate) fn open_provider_directory_for_discovery(
    _adapter: &dyn ObservedAgent,
    _path: &Path,
) -> Result<Option<std::fs::File>> {
    anyhow::bail!(
        "secure provider directory discovery is unavailable on this platform; import fails closed"
    )
}

fn securely_open_provider_file(
    adapter: &dyn ObservedAgent,
    path: &Path,
) -> Result<Option<(std::fs::File, String)>> {
    if !path.is_absolute() {
        return Ok(None);
    }
    let path = normalize_macos_system_directory_alias(path);
    for root in configured_provider_roots(adapter) {
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let file = open_beneath_no_follow(&root, relative)?;
        let source_id = relative.to_string_lossy().into_owned();
        return Ok(Some((file, source_id)));
    }
    Ok(None)
}

/// Preserve the pre-M4 live-capture behavior on platforms that do not expose
/// Unix descriptor-relative no-follow traversal. Historical import calls the
/// strict resolver below and still fails closed there; this compatibility
/// path is only for an already-running provider hook capture.
#[cfg(not(unix))]
fn compatibly_open_provider_file(
    adapter: &dyn ObservedAgent,
    path: &Path,
) -> Result<Option<(std::fs::File, String)>> {
    let canonical = path
        .canonicalize()
        .context("canonicalize live provider transcript")?;
    let Some(root) = provider_root_containing(adapter, &canonical) else {
        return Ok(None);
    };
    let relative = canonical
        .strip_prefix(&root)
        .context("bound live provider transcript to protected root")?;
    let file = std::fs::File::open(&canonical).context("open live provider transcript")?;
    if !file
        .metadata()
        .context("inspect live provider transcript")?
        .is_file()
    {
        anyhow::bail!("live provider transcript source is not a regular file");
    }
    Ok(Some((file, relative.to_string_lossy().into_owned())))
}

fn open_provider_file_for_capture(
    adapter: &dyn ObservedAgent,
    path: &Path,
    strict_import: bool,
) -> Result<Option<(std::fs::File, String)>> {
    #[cfg(unix)]
    {
        let _ = strict_import;
        securely_open_provider_file(adapter, path)
    }
    #[cfg(not(unix))]
    {
        if strict_import {
            securely_open_provider_file(adapter, path)
        } else {
            compatibly_open_provider_file(adapter, path)
        }
    }
}

#[cfg(test)]
mod test_support {
    use std::{
        sync::{Mutex, OnceLock, mpsc},
        time::Duration,
    };

    use anyhow::{Result, anyhow};

    struct SecureOpenPause {
        reached: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }

    fn pause_slot() -> &'static Mutex<Option<SecureOpenPause>> {
        static PAUSE: OnceLock<Mutex<Option<SecureOpenPause>>> = OnceLock::new();
        PAUSE.get_or_init(|| Mutex::new(None))
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub(super) struct PauseReset;

    impl Drop for PauseReset {
        fn drop(&mut self) {
            let mut pause = pause_slot()
                .lock()
                .expect("secure-open test pause lock is not poisoned");
            pause.take();
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn install_secure_open_pause() -> (mpsc::Receiver<()>, mpsc::Sender<()>, PauseReset)
    {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let mut pause = pause_slot()
            .lock()
            .expect("secure-open test pause lock is not poisoned");
        assert!(
            pause.is_none(),
            "a secure-open test pause is already installed"
        );
        *pause = Some(SecureOpenPause {
            reached: reached_tx,
            resume: resume_rx,
        });
        (reached_rx, resume_tx, PauseReset)
    }

    pub(super) fn pause_before_secure_open() -> Result<()> {
        let pause = pause_slot()
            .lock()
            .expect("secure-open test pause lock is not poisoned")
            .take();
        let Some(pause) = pause else {
            return Ok(());
        };
        pause
            .reached
            .send(())
            .map_err(|_| anyhow!("secure-open test lost its pause observer"))?;
        pause
            .resume
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("secure-open test pause was not resumed"))?;
        Ok(())
    }
}

#[cfg(test)]
fn import_test_pause_before_secure_open() -> Result<()> {
    test_support::pause_before_secure_open()
}

#[cfg(not(test))]
fn import_test_pause_before_secure_open() -> Result<()> {
    Ok(())
}

/// Migration-period provider-root containment precheck (ADR-DR-13). Returns
/// true when `path` canonicalises to a location inside the adapter's own
/// transcript root. Not a final TOCTOU boundary on its own — the resolver
/// additionally opens the handle once and reads from the descriptor.
pub fn transcript_path_within_provider_root(adapter: &dyn ObservedAgent, path: &Path) -> bool {
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    provider_root_containing(adapter, &canonical_path).is_some()
}

/// The unified writer read entry point (ADR-DR-02).
///
/// Returns:
/// - `Ok(Some(File { … }))` when the ctx carries a `transcript_path` that
///   passes the provider-root precheck and opens successfully — the handle is
///   opened here, once.
/// - `Ok(None)` when there is no path, the path is untrusted (outside the
///   provider root), or the file is absent. The writer treats this as "no
///   transcript" and falls back to the redacted prompt, preserving existing
///   fail-open-on-absent semantics while staying fail-closed on untrusted
///   paths.
/// - `Err(_)` only on an unexpected I/O error opening a trusted, present path.
fn resolve_transcript_source_with_policy(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
    require_pinned_open: bool,
    import_test_pause: bool,
    preparation_deadline: Option<std::time::Instant>,
    run_preparer: bool,
) -> Result<Option<TranscriptSource>> {
    let Some(path) = ctx.transcript_path.as_deref() else {
        return Ok(None);
    };
    if import_test_pause {
        import_test_pause_before_secure_open()?;
    }
    match open_provider_file_for_capture(adapter, path, require_pinned_open) {
        Ok(Some((file, source_id))) => {
            let authorized = AuthorizedTranscriptFile { file };
            // DR-01/ADR-DR-13: preparation consumes the exact pinned
            // descriptor. It cannot reopen a path that may have been swapped
            // after authorization.
            if run_preparer
                && let Some(preparer) = adapter.as_transcript_preparer()
                && preparer
                    .prepare_transcript(ctx, authorized.descriptor(), preparation_deadline)
                    .is_err()
            {
                tracing::warn!(
                    reason = "transcript_preparer_failed",
                    "transcript preparer failed; continuing"
                );
            }
            Ok(Some(TranscriptSource::File {
                file: authorized,
                source_id,
                auth: ProviderRootAuthorized(()),
            }))
        }
        Ok(None) => Ok(None),
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "open authorized transcript for '{}'",
                adapter.provider_name()
            )
        }),
    }
}

/// Resolve a live source while preserving the distinction between absence and
/// a rejected path. The returned classification remains deliberately narrow:
/// callers must not recover or persist the original locator.
fn resolve_transcript_source_classified_with_policy(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
    require_pinned_open: bool,
    import_test_pause: bool,
    preparation_deadline: Option<std::time::Instant>,
    run_preparer: bool,
) -> Result<TranscriptSourceResolution> {
    let Some(path) = ctx.transcript_path.as_deref() else {
        return Ok(TranscriptSourceResolution::Absent);
    };

    // Classify an explicitly supplied path before secure opening. `try_exists`
    // is only diagnostic classification; authorization and opening still occur
    // through the descriptor-pinned resolver below.
    match path.try_exists() {
        Ok(false) => return Ok(TranscriptSourceResolution::Absent),
        Ok(true) => {}
        Err(error) => return Err(error).context("inspect transcript source candidate"),
    }
    if !transcript_path_within_provider_root(adapter, path) {
        return Ok(TranscriptSourceResolution::Untrusted);
    }

    match resolve_transcript_source_with_policy(
        adapter,
        ctx,
        require_pinned_open,
        import_test_pause,
        preparation_deadline,
        run_preparer,
    ) {
        Ok(Some(source)) => Ok(TranscriptSourceResolution::Authorized(source)),
        // A present path that could not be opened beneath the lexical
        // provider root is untrusted (for example, an outside symlink whose
        // canonical target points back inside the root). Recheck only to
        // preserve the absent classification for a genuine remove race.
        Ok(None) => match path.try_exists() {
            Ok(true) => Ok(TranscriptSourceResolution::Untrusted),
            Ok(false) => Ok(TranscriptSourceResolution::Absent),
            Err(error) => Err(error).context("recheck transcript source after secure open"),
        },
        Err(error) => Err(error),
    }
}

/// Resolve a source for existing live hook capture. Unix receives the same
/// descriptor-relative no-follow protection as import; other platforms keep
/// the prior canonical-path compatibility behavior.
pub fn resolve_transcript_source(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, false, false, None, true)
}

/// Resolve one live source with a cooperative preparation deadline and a
/// classified safe outcome.
///
/// Live capture deliberately takes the same strict descriptor-pinned path as
/// historical import.  The former compatibility branch on non-Unix platforms
/// canonicalized a path and then re-opened it, which let a pathname swap turn
/// a successfully checked provider source into an arbitrary file.  A platform
/// without an equivalent no-follow handle walk therefore yields a safe
/// read-error/partial snapshot instead of weakening the authorization claim.
/// This is crate-private because only capture services should need to
/// distinguish rejected from absent sources.
pub(crate) fn resolve_live_transcript_source_until(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
    deadline: Option<std::time::Instant>,
) -> Result<TranscriptSourceResolution> {
    // A deadline-bound live snapshot passes its held descriptor to the
    // killable helper, which owns Claude's flush preparation and raw read.
    // The synchronous compatibility path retains the historical in-process
    // preparer because it never crosses the descriptor helper boundary.
    resolve_transcript_source_classified_with_policy(
        adapter,
        ctx,
        true,
        false,
        deadline,
        deadline.is_none(),
    )
}

/// Resolve a historical-import source. Platforms without an equivalent to
/// Unix descriptor-relative no-follow traversal fail closed rather than
/// weakening the import authorization boundary.
pub fn resolve_import_transcript_source(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, true, true, None, true)
}

#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn resolve_import_transcript_source_until(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
    deadline: std::time::Instant,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, true, true, Some(deadline), true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serial_test::serial;

    use super::*;
    use crate::internal::ai::observed_agents::{
        AgentKind, builtin::ClaudeCodeObservedAgent, capability::TranscriptPreparer,
    };

    #[test]
    fn authorized_source_debug_omits_session_digest_and_path() {
        let bytes = b"unique-transcript-debug-marker";
        let authorization = ExportAuthorized::issue("claude_code", "private-session-marker", bytes);
        let debug = format!("{authorization:?}");
        assert!(!debug.contains("private-session-marker"));
        assert!(!debug.contains(&authorization.content_digest));
        assert!(debug.contains("<redacted>"));

        let file = tempfile::NamedTempFile::new().expect("create authorized source fixture");
        let path = file.path().display().to_string();
        let authorized = AuthorizedTranscriptFile {
            file: file.reopen().expect("reopen fixture"),
        };
        let debug = format!("{authorized:?}");
        assert!(!debug.contains(&path));
        assert!(debug.contains("<descriptor>"));
    }

    #[derive(Default)]
    struct CountingPreparer {
        calls: AtomicUsize,
    }

    impl ObservedAgent for CountingPreparer {
        fn provider_kind(&self) -> AgentKind {
            AgentKind::ClaudeCode
        }

        fn provider_name(&self) -> &'static str {
            "counting-preparer"
        }

        fn read_transcript(&self, _session: &AgentSessionCtx) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn protected_dirs(&self) -> &'static [&'static str] {
            &[".claude"]
        }

        fn as_transcript_preparer(&self) -> Option<&dyn TranscriptPreparer> {
            Some(self)
        }
    }

    impl TranscriptPreparer for CountingPreparer {
        fn prepare_transcript(
            &self,
            _session: &AgentSessionCtx,
            _file: &std::fs::File,
            _deadline: Option<std::time::Instant>,
        ) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// RAII guard that points `LIBRA_TEST_HOME` at `path` and restores the
    /// prior value on drop. Env mutation is `unsafe` and the tests carry
    /// `#[serial(env)]` so it cannot race other env readers.
    fn test_ctx(path: Option<PathBuf>) -> AgentSessionCtx {
        AgentSessionCtx {
            session_id: "claude_code__t".to_string(),
            provider_session_id: "t".to_string(),
            working_dir: PathBuf::from("/tmp"),
            transcript_path: path,
        }
    }

    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("LIBRA_TEST_HOME");
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self { prior }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var("LIBRA_TEST_HOME", v),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    fn make_claude_transcript(home: &Path, name: &str, content: &[u8]) -> PathBuf {
        let dir = home.join(".claude").join("projects").join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_none_when_no_path() {
        let agent = ClaudeCodeObservedAgent::new();
        let adapter: &dyn ObservedAgent = &agent;
        assert!(
            resolve_transcript_source(adapter, &test_ctx(None))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn transcript_preparer_failure_telemetry_is_content_free() {
        let production = include_str!("transcript_source.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("transcript-source test module delimiter exists");
        let resolver = production
            .split("fn resolve_transcript_source_with_policy(")
            .nth(1)
            .and_then(|entry| {
                entry
                    .split("/// Resolve a source for existing live hook capture.")
                    .next()
            })
            .expect("transcript source resolver exists");
        assert!(
            resolver.contains("reason = \"transcript_preparer_failed\"")
                && !resolver.contains("error = %")
                && !resolver.contains("format!(\"{err:#}\")"),
            "preparer failure telemetry must use a fixed reason, not an error chain that can contain source data"
        );
    }

    #[test]
    #[serial(env)]
    fn resolve_none_when_untrusted_path() {
        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        // A real file that lives OUTSIDE ~/.claude — the security gate must
        // refuse it (fail-closed) so the writer falls back to the prompt.
        let outside = home.path().join("evil.jsonl");
        std::fs::write(&outside, b"secret").unwrap();
        let agent = CountingPreparer::default();
        let adapter: &dyn ObservedAgent = &agent;
        assert!(
            resolve_transcript_source(adapter, &test_ctx(Some(outside.clone())))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            agent.calls.load(Ordering::SeqCst),
            0,
            "provider-root rejection must happen before any preparer read"
        );
    }

    #[test]
    #[serial(env)]
    fn resolve_file_reads_bytes_and_root_relative_source_id() {
        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        let path = make_claude_transcript(home.path(), "s.jsonl", b"hello");
        let agent = ClaudeCodeObservedAgent::new();
        let adapter: &dyn ObservedAgent = &agent;
        let src = resolve_transcript_source(adapter, &test_ctx(Some(path.clone())))
            .unwrap()
            .expect("trusted path yields a File source");
        match src {
            TranscriptSource::File {
                mut file,
                source_id,
                ..
            } => {
                assert_eq!(
                    file.read_bounded(TRANSCRIPT_READ_HARD_CAP_BYTES).unwrap(),
                    b"hello"
                );
                // Provider-root-relative identity, never an absolute home path.
                assert!(!source_id.starts_with('/'));
                assert!(source_id.contains("projects"));
                assert!(source_id.ends_with("s.jsonl"));
                assert!(!source_id.contains(home.path().to_string_lossy().as_ref()));
            }
            _ => panic!("expected File source"),
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn live_resolution_classifies_outside_symlink_into_root_as_untrusted() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::set(home.path());
        let trusted = make_claude_transcript(home.path(), "s.jsonl", b"trusted");
        let outside_link = outside.path().join("alias.jsonl");
        std::os::unix::fs::symlink(&trusted, &outside_link).unwrap();
        let agent = ClaudeCodeObservedAgent::new();

        let resolution =
            resolve_live_transcript_source_until(&agent, &test_ctx(Some(outside_link)), None)
                .unwrap();

        assert!(matches!(resolution, TranscriptSourceResolution::Untrusted));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial(env)]
    fn secure_import_accepts_the_fixed_tmp_system_alias() {
        // `/tmp` is an OS-owned symlink to `/private/tmp` on macOS. The
        // provider root and candidate path use its lexical form here so this
        // exercises normalization before the no-follow descriptor walk.
        let fixture = tempfile::Builder::new()
            .prefix("libra-transcript-tmp-alias-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let real_home = fixture.path().join("home");
        let real_path = make_claude_transcript(&real_home, "s.jsonl", b"trusted");
        let relative = real_path.strip_prefix(&real_home).unwrap();
        let alias_home = Path::new("/tmp")
            .join(fixture.path().file_name().unwrap())
            .join("home");
        let alias_path = alias_home.join(relative);
        let _guard = HomeGuard::set(&alias_home);
        let agent = ClaudeCodeObservedAgent::new();

        assert!(
            open_provider_directory_for_discovery(
                &agent,
                &alias_home.join(".claude").join("projects").join("proj"),
            )
            .unwrap()
            .is_some(),
            "the fixed /tmp alias must remain available for secure discovery"
        );

        let source = resolve_import_transcript_source(&agent, &test_ctx(Some(alias_path)))
            .unwrap()
            .expect("the fixed /tmp alias must remain an authorized provider root");
        match source {
            TranscriptSource::File { mut file, .. } => {
                assert_eq!(
                    file.read_bounded(TRANSCRIPT_READ_HARD_CAP_BYTES).unwrap(),
                    b"trusted"
                );
            }
            _ => panic!("expected File source"),
        }
    }

    // On Unix a held descriptor keeps reading the original inode even after the
    // path is unlinked and replaced, so a post-authorization symlink/path swap
    // cannot change the bytes the writer reads (the TOCTOU invariant).
    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn open_handle_survives_path_swap() {
        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        let path = make_claude_transcript(home.path(), "s.jsonl", b"ORIGINAL");
        let agent = ClaudeCodeObservedAgent::new();
        let adapter: &dyn ObservedAgent = &agent;
        let src = resolve_transcript_source(adapter, &test_ctx(Some(path.clone())))
            .unwrap()
            .unwrap();
        // Swap the path to a NEW file with different content after auth.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"SWAPPED-EVIL-CONTENT").unwrap();
        match src {
            TranscriptSource::File { mut file, .. } => {
                assert_eq!(
                    file.read_bounded(TRANSCRIPT_READ_HARD_CAP_BYTES).unwrap(),
                    b"ORIGINAL",
                    "held descriptor must not observe the post-auth path swap"
                );
            }
            _ => panic!("expected File source"),
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn secure_open_pause_rechecks_a_swapped_source_without_env() {
        let home = tempfile::tempdir().expect("create provider-root fixture");
        let outside = tempfile::tempdir().expect("create outside-source fixture");
        let _guard = HomeGuard::set(home.path());
        let path = make_claude_transcript(home.path(), "pre-open-swap.jsonl", b"ORIGINAL");
        let outside_source = outside.path().join("outside.jsonl");
        std::fs::write(&outside_source, b"OUTSIDE").expect("write outside source");
        let (reached, resume, _pause_reset) = test_support::install_secure_open_pause();
        let worker_path = path.clone();

        let worker = std::thread::spawn(move || {
            let agent = ClaudeCodeObservedAgent::new();
            resolve_import_transcript_source(&agent, &test_ctx(Some(worker_path)))
        });
        reached
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("secure open must pause after authorizing the test seam");

        std::fs::remove_file(&path).expect("remove checked source");
        std::os::unix::fs::symlink(&outside_source, &path)
            .expect("replace checked source with symlink");
        resume.send(()).expect("resume protected open");

        let result = worker.join().expect("join secure-open worker");
        assert!(
            result.is_err(),
            "descriptor-relative no-follow open must reject a source swapped after the pause"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn provider_root_component_symlink_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        std::fs::create_dir_all(home.path().join(".claude").join("projects")).unwrap();
        std::fs::write(outside.path().join("s.jsonl"), b"OUTSIDE").unwrap();
        std::os::unix::fs::symlink(
            outside.path(),
            home.path().join(".claude").join("projects").join("swapped"),
        )
        .unwrap();
        let path = home
            .path()
            .join(".claude")
            .join("projects")
            .join("swapped")
            .join("s.jsonl");
        let agent = ClaudeCodeObservedAgent::new();
        assert!(
            resolve_import_transcript_source(&agent, &test_ctx(Some(path))).is_err(),
            "descriptor-relative traversal must reject a symlinked component"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn provider_root_intermediate_component_symlink_is_rejected() {
        let container = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let real_home = outside.path().join("home");
        make_claude_transcript(&real_home, "outside.jsonl", b"OUTSIDE");
        let linked_home = container.path().join("linked-home");
        std::os::unix::fs::symlink(&real_home, &linked_home).unwrap();
        let transcript = linked_home.join(".claude/projects/proj/outside.jsonl");
        let _guard = HomeGuard::set(&linked_home);
        let agent = ClaudeCodeObservedAgent::new();

        assert!(
            resolve_import_transcript_source(&agent, &test_ctx(Some(transcript))).is_err(),
            "an intermediate symlink in the absolute provider root must fail closed"
        );
        assert!(
            open_provider_directory_for_discovery(
                &agent,
                &linked_home.join(".claude/projects/proj")
            )
            .is_err(),
            "pre-consent discovery must reject the same intermediate symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn pinned_provider_directory_survives_root_rename_and_symlink_swap() {
        let container = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let home = container.path().join("home");
        let project = home.join(".claude/projects/proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("original.jsonl"), b"ORIGINAL").unwrap();
        std::fs::write(outside.path().join("outside.jsonl"), b"OUTSIDE").unwrap();
        let _guard = HomeGuard::set(&home);
        let agent = ClaudeCodeObservedAgent::new();
        let directory = open_provider_directory_for_discovery(&agent, &project)
            .unwrap()
            .expect("open pinned project directory");

        std::fs::rename(home.join(".claude"), home.join(".claude-original")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.join(".claude")).unwrap();
        let names = read_dir_pinned_provider_directory(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("original.jsonl")]);
    }

    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn fifo_source_is_rejected_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, time::Duration};

        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        let path = home
            .path()
            .join(".claude")
            .join("projects")
            .join("proj")
            .join("blocked.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        let agent = ClaudeCodeObservedAgent::new();
        assert!(resolve_import_transcript_source(&agent, &test_ctx(Some(path))).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "FIFO authorization must not wait for a writer"
        );
    }

    #[test]
    #[serial(env)]
    fn read_bounded_refuses_oversize() {
        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        let path = make_claude_transcript(home.path(), "big.jsonl", b"0123456789");
        let agent = ClaudeCodeObservedAgent::new();
        let adapter: &dyn ObservedAgent = &agent;
        let src = resolve_transcript_source(adapter, &test_ctx(Some(path.clone())))
            .unwrap()
            .unwrap();
        match src {
            TranscriptSource::File { mut file, .. } => {
                assert!(
                    file.read_bounded(4).is_err(),
                    "oversize transcript must be refused, not truncated"
                );
            }
            _ => panic!("expected File source"),
        }
    }

    #[test]
    fn bytes_source_carries_digest_bound_export_tag() {
        // `ExportAuthorized` can only be minted crate-side via `issue`, which
        // binds the tag to the exact bytes; `matches` re-verifies session AND
        // digest, so a tag cannot authorize different bytes.
        let bytes = b"exported".to_vec();
        let auth = ExportAuthorized::issue("opencode", "opencode__abc", &bytes);
        assert!(auth.matches("opencode", "opencode__abc", &bytes));
        assert!(
            !auth.matches("opencode", "opencode__abc", b"tampered"),
            "digest binding must reject different bytes"
        );
        assert!(
            !auth.matches("opencode", "opencode__other", &bytes),
            "session binding must reject a different session"
        );
        let src = TranscriptSource::Bytes { bytes, auth };
        match src {
            TranscriptSource::Bytes { bytes, auth } => {
                assert!(auth.matches("opencode", "opencode__abc", &bytes));
                assert_eq!(auth.agent_kind(), "opencode");
            }
            _ => panic!("expected Bytes source"),
        }
    }
}
