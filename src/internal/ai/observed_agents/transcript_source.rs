//! DR-04a — the unified `TranscriptSource` seam (ADR-DR-02).
//!
//! This is the **single writer read entry point** for external-agent
//! transcript content. Both the live checkpoint writer
//! (`hooks::runtime::write_committed_checkpoint`) and — once it lands (M4) —
//! the import writer resolve their bytes through
//! [`resolve_transcript_source`], never by re-opening a path themselves.
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
//!   [`ExportAuthorized`] tag. This shape is **only** constructed by the
//!   OpenCode export bridge (DR-04b) after a trusted, sandboxed export; there
//!   is no public way to forge the tag, so the writer will not treat an
//!   arbitrary `&[u8]` as a trusted source.
//!
//! Security note (ADR-DR-13): the provider-root containment check here
//! ([`transcript_path_within_provider_root`]) is the **migration-period
//! precheck**. The final fd-relative `openat2(RESOLVE_BENEATH | …)` safe-open
//! lands with DR-05b; until then the resolver opens the path once (after the
//! precheck) and hands the writer the open handle, which already removes the
//! re-open-by-path TOCTOU on the read side.

use std::{
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use thiserror::Error;

use crate::internal::ai::observed_agents::{AgentSessionCtx, ObservedAgent};

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

/// Proof token that a [`TranscriptSource::Bytes`] payload came from this
/// process's own trusted export bridge (DR-04b). Fields are private and the
/// only constructor is crate-scoped [`ExportAuthorized::issue`], which binds
/// the tag to the exact bytes via SHA-256 — so no caller outside this crate
/// can mint a tag, and a tag cannot be re-attached to different bytes: the
/// writer re-verifies with [`ExportAuthorized::matches`].
#[derive(Debug, Clone)]
pub struct ExportAuthorized {
    agent_kind: String,
    session_id: String,
    content_digest: String,
}

impl ExportAuthorized {
    /// Mint an authorization tag for freshly exported `bytes`. Crate-scoped:
    /// only the verified export bridge (DR-04b) may issue tags.
    // Production caller lands with the DR-04b export bridge (M3); the digest
    // binding is unit-tested until then.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn issue(agent_kind: &str, session_id: &str, bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            agent_kind: agent_kind.to_string(),
            session_id: session_id.to_string(),
            content_digest: hex::encode(Sha256::digest(bytes)),
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

    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }
}

/// A transcript file that has already been safely opened inside the provider
/// root. The writer reads from the held descriptor; the path is retained only
/// for diagnostics / `source_id` derivation and is **never** re-opened.
#[derive(Debug)]
pub struct AuthorizedTranscriptFile {
    file: std::fs::File,
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
        let mut buf = Vec::new();
        // Read one past the cap so an oversize file is detected, not silently
        // truncated; `take` still bounds memory on the hook path.
        if let Err(error) = self
            .file
            .by_ref()
            .take(cap.saturating_add(1))
            .read_to_end(&mut buf)
            .context("read authorized transcript handle")
        {
            let bytes_read = buf.len() as u64;
            return (Err(error), bytes_read);
        }
        let bytes_read = buf.len() as u64;
        if bytes_read > cap {
            return (
                Err(TranscriptReadError::ExceedsCap { cap }.into()),
                bytes_read,
            );
        }
        (Ok(buf), bytes_read)
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
#[derive(Debug)]
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

/// Resolve the provider root that contains `canonical_path`, if any. Mirrors
/// the Codex `$CODEX_HOME` relocation honored elsewhere in the codex chain so a
/// relocated home is not silently captured with an empty transcript.
fn normalize_macos_var_alias(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        path.strip_prefix("/var")
            .map(|suffix| Path::new("/private/var").join(suffix))
            .unwrap_or_else(|_| path.to_path_buf())
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
        let root = normalize_macos_var_alias(&root).canonicalize().ok()?;
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
            normalize_macos_var_alias(&root)
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

/// Address a held directory descriptor for safe enumeration while retaining
/// the descriptor for the entire walk. Every descendant lookup is therefore
/// rooted at the object opened by [`open_provider_directory_for_discovery`],
/// even if an attacker swaps the provider's pathname concurrently.
#[cfg(target_os = "linux")]
pub(crate) fn pinned_provider_directory_path(directory: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd;

    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn pinned_provider_directory_path(directory: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd;

    PathBuf::from(format!("/dev/fd/{}", directory.as_raw_fd()))
}

#[cfg(not(unix))]
pub(crate) fn pinned_provider_directory_path(_directory: &std::fs::File) -> PathBuf {
    PathBuf::new()
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
    let path = normalize_macos_var_alias(path);
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

fn import_test_pause_before_secure_open() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Ok(ready_path) = std::env::var("LIBRA_TEST_IMPORT_SECURE_OPEN_READY_FILE") else {
        return Ok(());
    };
    let continue_path = std::env::var("LIBRA_TEST_IMPORT_SECURE_OPEN_CONTINUE_FILE")
        .context("secure-open pause requires a continue-file path")?;
    std::fs::write(&ready_path, b"ready").context("publish test-only secure-open import pause")?;
    while !Path::new(&continue_path).exists() {
        // Deliberately ignore the in-process deadline: this models an
        // uninterruptible filesystem open. Historical import must remain
        // bounded because this code executes only in its killable helper.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
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
    strict_import: bool,
    preparation_deadline: Option<std::time::Instant>,
) -> Result<Option<TranscriptSource>> {
    let Some(path) = ctx.transcript_path.as_deref() else {
        return Ok(None);
    };
    if strict_import {
        import_test_pause_before_secure_open()?;
    }
    match open_provider_file_for_capture(adapter, path, strict_import) {
        Ok(Some((file, source_id))) => {
            let authorized = AuthorizedTranscriptFile { file };
            // DR-01/ADR-DR-13: preparation consumes the exact pinned
            // descriptor. It cannot reopen a path that may have been swapped
            // after authorization.
            if let Some(preparer) = adapter.as_transcript_preparer()
                && let Err(err) =
                    preparer.prepare_transcript(ctx, authorized.descriptor(), preparation_deadline)
            {
                tracing::warn!(error = %format!("{err:#}"), "transcript preparer failed; continuing");
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

/// Resolve a source for existing live hook capture. Unix receives the same
/// descriptor-relative no-follow protection as import; other platforms keep
/// the prior canonical-path compatibility behavior.
pub fn resolve_transcript_source(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, false, None)
}

/// Resolve a historical-import source. Platforms without an equivalent to
/// Unix descriptor-relative no-follow traversal fail closed rather than
/// weakening the import authorization boundary.
pub fn resolve_import_transcript_source(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, true, None)
}

pub(crate) fn resolve_import_transcript_source_until(
    adapter: &dyn ObservedAgent,
    ctx: &AgentSessionCtx,
    deadline: std::time::Instant,
) -> Result<Option<TranscriptSource>> {
    resolve_transcript_source_with_policy(adapter, ctx, true, Some(deadline))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serial_test::serial;

    use super::*;
    use crate::internal::ai::observed_agents::{
        AgentKind, builtin::ClaudeCodeObservedAgent, capability::TranscriptPreparer,
    };

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
    /// `#[serial]` so it cannot race other env readers.
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
    #[serial]
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
    #[serial]
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

    // On Unix a held descriptor keeps reading the original inode even after the
    // path is unlinked and replaced, so a post-authorization symlink/path swap
    // cannot change the bytes the writer reads (the TOCTOU invariant).
    #[cfg(unix)]
    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
        let pinned = pinned_provider_directory_path(&directory);
        let names = std::fs::read_dir(pinned)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("original.jsonl")]);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
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
    #[serial]
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
