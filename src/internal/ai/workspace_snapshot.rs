//! Workspace snapshot utilities used to compare task worktrees against a baseline.
//!
//! Boundary: snapshots record relative paths, file content hashes, metadata kind, and
//! deletion state without following symlinks outside the workspace. Orchestrator
//! workspace tests cover symlink, deletion, and changed-file edge cases.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use git_internal::{hash::ObjectHash, internal::object::blob::Blob};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use crate::internal::ai::generated_artifacts;

pub(crate) const SNAPSHOT_CONTENT_MAX_FILE_BYTES: usize = 1024 * 1024;
pub(crate) const SNAPSHOT_CONTENT_MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const FINGERPRINT_MAX_ENTRIES: usize = 1_000_000;
const FINGERPRINT_MAX_PATH_BYTES: usize = 128 * 1024 * 1024;
const FINGERPRINT_MAX_DURATION: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct FingerprintBudget {
    max_entries: usize,
    max_path_bytes: usize,
    max_duration: Duration,
}

trait FingerprintClock {
    fn elapsed(&self, started_at: Instant, checkpoint: FingerprintCheckpoint) -> Duration;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FingerprintCheckpoint {
    General,
    AfterWalkStep,
    AfterWalkEof,
    AfterEntryMetadata,
    AfterSymlinkRead,
    AfterContentPreMetadata,
    AfterFileOpen,
    AfterFileRead,
    AfterFileEof,
    AfterContentPostMetadata,
    AfterSecureRootOpen,
    AfterSecureParentOpen,
    AfterSecureLeafOpen,
    AfterSecureDescriptorMetadata,
    AfterSecurePathMetadata,
    AfterSecureIdentity,
    #[cfg(windows)]
    AfterSecureFinalPath,
    AfterSecureSymlinkRead,
    AfterMetadataPostLstat,
    AfterSort,
    BeforeManifestReturn,
    BeforeExactReturn,
    BeforeMetadataReturn,
}

struct SystemFingerprintClock;

impl FingerprintClock for SystemFingerprintClock {
    fn elapsed(&self, started_at: Instant, _checkpoint: FingerprintCheckpoint) -> Duration {
        started_at.elapsed()
    }
}

const DEFAULT_FINGERPRINT_BUDGET: FingerprintBudget = FingerprintBudget {
    max_entries: FINGERPRINT_MAX_ENTRIES,
    max_path_bytes: FINGERPRINT_MAX_PATH_BYTES,
    max_duration: FINGERPRINT_MAX_DURATION,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) entries: BTreeMap<PathBuf, WorkspaceEntry>,
    pub(crate) file_contents: BTreeMap<PathBuf, Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceEntry {
    File(ObjectHash),
    Symlink(PathBuf),
}

pub(crate) fn snapshot_workspace(root: &Path) -> io::Result<WorkspaceSnapshot> {
    snapshot_workspace_inner(root, false)
}

/// Stable digest of the ignore-aware workspace view used by AI tool safety.
///
/// Paths, file blob hashes, and symlink targets are length-delimited so a
/// checkout binding cannot confuse adjacent fields. Protected repository
/// metadata and generated/cache directories follow the same exclusions as
/// task-worktree mutation detection.
pub(crate) fn workspace_snapshot_fingerprint(root: &Path) -> io::Result<String> {
    workspace_snapshot_fingerprint_with_budget(root, DEFAULT_FINGERPRINT_BUDGET)
}

fn workspace_snapshot_fingerprint_with_budget(
    root: &Path,
    budget: FingerprintBudget,
) -> io::Result<String> {
    workspace_snapshot_fingerprint_with_budget_started_at(root, budget, Instant::now())
}

fn workspace_snapshot_fingerprint_with_budget_started_at(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
) -> io::Result<String> {
    workspace_snapshot_fingerprint_with_budget_clock(
        root,
        budget,
        started_at,
        &SystemFingerprintClock,
    )
}

fn workspace_snapshot_fingerprint_with_budget_clock<C: FingerprintClock>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
) -> io::Result<String> {
    workspace_snapshot_fingerprint_with_budget_clock_and_open_hook(
        root,
        budget,
        started_at,
        clock,
        |_| Ok(()),
    )
}

fn workspace_snapshot_fingerprint_with_budget_clock_and_open_hook<C, H>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
    mut before_entry_open: H,
) -> io::Result<String>
where
    C: FingerprintClock,
    H: FnMut(&Path) -> io::Result<()>,
{
    let paths = collect_fingerprint_paths(root, budget, started_at, clock)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    for relative in paths {
        enforce_fingerprint_budget(started_at, 0, budget, clock, FingerprintCheckpoint::General)?;
        let path = root.join(&relative);
        let entry_metadata = fs::symlink_metadata(&path)?;
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterEntryMetadata,
        )?;
        let file_type = entry_metadata.file_type();
        update_digest_field(&mut digest, relative.as_os_str().as_encoded_bytes());
        before_entry_open(&path)?;
        let mut io_checkpoint =
            |checkpoint| enforce_fingerprint_budget(started_at, 0, budget, clock, checkpoint);
        if file_type.is_symlink() {
            digest.update(b"symlink\0");
            let target = read_stable_workspace_symlink(
                root,
                &relative,
                &entry_metadata,
                &mut io_checkpoint,
            )?;
            enforce_fingerprint_budget(
                started_at,
                0,
                budget,
                clock,
                FingerprintCheckpoint::AfterSymlinkRead,
            )?;
            update_digest_field(&mut digest, target.as_os_str().as_encoded_bytes());
            continue;
        }
        if !file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "workspace contains unsupported non-regular entry '{}'",
                    relative.display()
                ),
            ));
        }
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterContentPreMetadata,
        )?;
        digest.update(b"file\0");
        let mut file_digest = Sha256::new();
        let mut opened = open_stable_workspace_regular_file(
            root,
            &relative,
            &entry_metadata,
            &mut io_checkpoint,
        )?;
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterFileOpen,
        )?;
        let mut read_bytes = 0u64;
        loop {
            enforce_fingerprint_budget(
                started_at,
                0,
                budget,
                clock,
                FingerprintCheckpoint::General,
            )?;
            let read = opened.file.read(&mut buffer)?;
            enforce_fingerprint_budget(
                started_at,
                0,
                budget,
                clock,
                if read == 0 {
                    FingerprintCheckpoint::AfterFileEof
                } else {
                    FingerprintCheckpoint::AfterFileRead
                },
            )?;
            if read == 0 {
                break;
            }
            read_bytes = read_bytes.saturating_add(read as u64);
            file_digest.update(&buffer[..read]);
        }
        opened.verify_after_read(root, &relative, read_bytes, &mut io_checkpoint)?;
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterContentPostMetadata,
        )?;
        update_digest_field(&mut digest, &file_digest.finalize());
    }
    enforce_fingerprint_budget(
        started_at,
        0,
        budget,
        clock,
        FingerprintCheckpoint::BeforeExactReturn,
    )?;
    Ok(hex::encode(digest.finalize()))
}

/// Stable, bounded-memory change token for an ignore-aware workspace view.
///
/// Unlike [`workspace_snapshot_fingerprint`], this does not read file bodies.
/// It is intended for interactive drift detection, where repeatedly hashing a
/// multi-gigabyte monorepo would make every approval transition block on disk
/// I/O. Paths, entry kinds, symlink targets, and change-sensitive metadata are
/// length-delimited. The result is a safety signal rather than a content or
/// object identity; execution still relies on the normal approval/sandbox/tool
/// gates.
pub(crate) fn workspace_snapshot_metadata_fingerprint(root: &Path) -> io::Result<String> {
    workspace_snapshot_metadata_fingerprint_with_budget(root, DEFAULT_FINGERPRINT_BUDGET)
}

fn workspace_snapshot_metadata_fingerprint_with_budget(
    root: &Path,
    budget: FingerprintBudget,
) -> io::Result<String> {
    workspace_snapshot_metadata_fingerprint_with_budget_started_at(root, budget, Instant::now())
}

fn workspace_snapshot_metadata_fingerprint_with_budget_started_at(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
) -> io::Result<String> {
    workspace_snapshot_metadata_fingerprint_with_budget_clock(
        root,
        budget,
        started_at,
        &SystemFingerprintClock,
    )
}

fn workspace_snapshot_metadata_fingerprint_with_budget_clock<C: FingerprintClock>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
) -> io::Result<String> {
    workspace_snapshot_metadata_fingerprint_with_budget_clock_and_lstat_hook(
        root,
        budget,
        started_at,
        clock,
        |_| Ok(()),
    )
}

fn workspace_snapshot_metadata_fingerprint_with_budget_clock_and_lstat_hook<C, H>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
    mut before_entry_lstat: H,
) -> io::Result<String>
where
    C: FingerprintClock,
    H: FnMut(&Path) -> io::Result<()>,
{
    let paths = collect_fingerprint_paths(root, budget, started_at, clock)?;
    let root_descriptor = crate::utils::beneath::open_root(root)?;
    enforce_fingerprint_budget(
        started_at,
        0,
        budget,
        clock,
        FingerprintCheckpoint::AfterSecureRootOpen,
    )?;
    let mut digest = Sha256::new();
    for relative in paths {
        enforce_fingerprint_budget(started_at, 0, budget, clock, FingerprintCheckpoint::General)?;
        before_entry_lstat(&root.join(&relative))?;
        let metadata = crate::utils::beneath::lstat_beneath(&root_descriptor, &relative)?;
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterEntryMetadata,
        )?;
        update_digest_field(&mut digest, relative.as_os_str().as_encoded_bytes());
        if metadata.is_symlink {
            digest.update(b"symlink\0");
            let mut io_checkpoint =
                |checkpoint| enforce_fingerprint_budget(started_at, 0, budget, clock, checkpoint);
            let target = read_stable_workspace_symlink_beneath(
                &root_descriptor,
                &relative,
                &mut io_checkpoint,
            )?;
            enforce_fingerprint_budget(
                started_at,
                0,
                budget,
                clock,
                FingerprintCheckpoint::AfterSymlinkRead,
            )?;
            update_digest_field(&mut digest, target.as_os_str().as_encoded_bytes());
        } else if metadata.is_file {
            digest.update(b"file\0");
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "workspace contains unsupported non-regular entry '{}'",
                    relative.display()
                ),
            ));
        }
        let metadata_after = crate::utils::beneath::lstat_beneath(&root_descriptor, &relative)?;
        enforce_fingerprint_budget(
            started_at,
            0,
            budget,
            clock,
            FingerprintCheckpoint::AfterMetadataPostLstat,
        )?;
        if !raw_lstat_matches(&metadata, &metadata_after) {
            return Err(workspace_file_changed_error(&relative));
        }
        update_raw_lstat_digest(&mut digest, &metadata);
    }
    enforce_fingerprint_budget(
        started_at,
        0,
        budget,
        clock,
        FingerprintCheckpoint::BeforeMetadataReturn,
    )?;
    Ok(hex::encode(digest.finalize()))
}

/// Capture the exact Execute authority and its metadata drift baseline from
/// one stable workspace interval. Exact scans on both sides of the test hook
/// make this independent of platform metadata fidelity; the outer metadata
/// scans bind the advisory drift token to that same content interval. The hook
/// is a no-op in production and gives tests a deterministic race injection
/// seam.
pub(crate) fn workspace_snapshot_stable_fingerprints(root: &Path) -> io::Result<(String, String)> {
    workspace_snapshot_stable_fingerprints_with_post_content_hook(root, || Ok(()))
}

pub(crate) fn workspace_snapshot_stable_fingerprints_with_post_content_hook<F>(
    root: &Path,
    post_content_hook: F,
) -> io::Result<(String, String)>
where
    F: FnOnce() -> io::Result<()>,
{
    let metadata_before = workspace_snapshot_metadata_fingerprint(root)?;
    let content_before = workspace_snapshot_fingerprint(root)?;
    post_content_hook()?;
    let content_after = workspace_snapshot_fingerprint(root)?;
    let metadata_after = workspace_snapshot_metadata_fingerprint(root)?;
    if metadata_before != metadata_after || content_before != content_after {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace changed while the Phase 1 content and metadata fingerprints were captured; retry after filesystem activity settles",
        ));
    }
    Ok((content_after, metadata_after))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkspaceFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
}

struct OpenedStableWorkspaceFile {
    file: fs::File,
    metadata: fs::Metadata,
    identity: WorkspaceFileIdentity,
}

impl OpenedStableWorkspaceFile {
    fn verify_after_read(
        &self,
        root: &Path,
        relative: &Path,
        read_bytes: u64,
        checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
    ) -> io::Result<()> {
        let descriptor_after = self.file.metadata()?;
        checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
        ensure_regular_workspace_file(&descriptor_after, relative)?;
        if read_bytes != self.metadata.len()
            || !stable_workspace_metadata_matches(&self.metadata, &descriptor_after)
        {
            return Err(workspace_file_changed_error(relative));
        }

        let path_after = fs::symlink_metadata(root.join(relative))?;
        checkpoint(FingerprintCheckpoint::AfterSecurePathMetadata)?;
        ensure_regular_workspace_file(&path_after, relative)?;
        if !stable_workspace_metadata_matches(&self.metadata, &path_after) {
            return Err(workspace_file_changed_error(relative));
        }

        // Re-resolve the complete relative path through the beneath helper.
        // This makes a persistent intermediate-directory swap fail even if a
        // path-based lstat happened to observe an attacker-controlled tree.
        let reopened = open_workspace_regular_file_descriptor(root, relative, checkpoint)?;
        let reopened_metadata = reopened.metadata()?;
        checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
        ensure_regular_workspace_file(&reopened_metadata, relative)?;
        let reopened_identity = workspace_file_identity(&reopened, &reopened_metadata)?;
        checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;
        if !stable_workspace_metadata_matches(&self.metadata, &reopened_metadata)
            || reopened_identity != self.identity
        {
            return Err(workspace_file_changed_error(relative));
        }
        Ok(())
    }
}

fn open_stable_workspace_regular_file(
    root: &Path,
    relative: &Path,
    path_before: &fs::Metadata,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<OpenedStableWorkspaceFile> {
    ensure_regular_workspace_file(path_before, relative)?;
    let file = open_workspace_regular_file_descriptor(root, relative, checkpoint)?;
    let descriptor_metadata = file.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    ensure_regular_workspace_file(&descriptor_metadata, relative)?;
    if !stable_workspace_metadata_matches(path_before, &descriptor_metadata) {
        return Err(workspace_file_changed_error(relative));
    }
    let identity = workspace_file_identity(&file, &descriptor_metadata)?;
    checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;

    // Keep the explicit post-open lstat in addition to descriptor-relative
    // resolution. It detects a final-component rename immediately, while the
    // second beneath open after reading validates the full path again.
    let path_after_open = fs::symlink_metadata(root.join(relative))?;
    checkpoint(FingerprintCheckpoint::AfterSecurePathMetadata)?;
    ensure_regular_workspace_file(&path_after_open, relative)?;
    if !stable_workspace_metadata_matches(&descriptor_metadata, &path_after_open) {
        return Err(workspace_file_changed_error(relative));
    }

    Ok(OpenedStableWorkspaceFile {
        file,
        metadata: descriptor_metadata,
        identity,
    })
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnixSymlinkState {
    device: u64,
    inode: u64,
    mode: u32,
    len: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
impl UnixSymlinkState {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    // libc's stat field widths differ across Unix targets; normalize them to
    // the platform-independent widths returned by MetadataExt.
    #[allow(clippy::unnecessary_cast)]
    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
            mode: stat.st_mode as u32,
            len: stat.st_size as u64,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: unix_stat_mtime_nanoseconds(stat),
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: unix_stat_ctime_nanoseconds(stat),
        }
    }

    #[allow(clippy::unnecessary_cast)]
    fn is_symlink(self) -> bool {
        (self.mode & libc::S_IFMT as u32) == libc::S_IFLNK as u32
    }
}

#[cfg(unix)]
fn read_stable_workspace_symlink(
    root: &Path,
    relative: &Path,
    path_before: &fs::Metadata,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    if !path_before.file_type().is_symlink() {
        return Err(workspace_file_changed_error(relative));
    }
    let root_descriptor = crate::utils::beneath::open_root(root)?;
    checkpoint(FingerprintCheckpoint::AfterSecureRootOpen)?;
    let expected = UnixSymlinkState::from_metadata(path_before);
    let (target, first_state) =
        read_workspace_symlink_once(&root_descriptor, relative, checkpoint)?;
    if first_state != expected {
        return Err(workspace_file_changed_error(relative));
    }

    let (target_after, second_state) =
        read_workspace_symlink_once(&root_descriptor, relative, checkpoint)?;
    if second_state != expected || target_after != target {
        return Err(workspace_file_changed_error(relative));
    }
    Ok(target)
}

#[cfg(unix)]
fn read_workspace_symlink_once(
    root_descriptor: &fs::File,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<(PathBuf, UnixSymlinkState)> {
    use std::{
        ffi::{CString, OsString},
        os::{
            fd::AsRawFd,
            unix::ffi::{OsStrExt, OsStringExt},
        },
    };

    validate_workspace_file_relative_path(relative)?;
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint symlink has no file name",
        )
    })?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint path contains an interior NUL byte",
        )
    })?;
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let directory = match parent {
        Some(parent) => {
            let directory = crate::utils::beneath::open_beneath(root_descriptor, parent)?;
            checkpoint(FingerprintCheckpoint::AfterSecureParentOpen)?;
            directory
        }
        None => {
            let directory = root_descriptor.try_clone()?;
            checkpoint(FingerprintCheckpoint::AfterSecureParentOpen)?;
            directory
        }
    };

    let before = unix_lstat_at(&directory, &name, checkpoint)?;
    if !before.is_symlink() {
        return Err(workspace_file_changed_error(relative));
    }
    let mut target = vec![0u8; 256];
    loop {
        let read = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                name.as_ptr(),
                target.as_mut_ptr().cast(),
                target.len(),
            )
        };
        checkpoint(FingerprintCheckpoint::AfterSecureSymlinkRead)?;
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = read as usize;
        if read < target.len() {
            target.truncate(read);
            break;
        }
        if target.len() >= 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "workspace symlink '{}' exceeds the 65536-byte fingerprint limit",
                    relative.display()
                ),
            ));
        }
        target.resize(target.len().saturating_mul(2).min(64 * 1024), 0);
    }
    let after = unix_lstat_at(&directory, &name, checkpoint)?;
    if before != after {
        return Err(workspace_file_changed_error(relative));
    }
    Ok((PathBuf::from(OsString::from_vec(target)), after))
}

#[cfg(unix)]
fn unix_lstat_at(
    directory: &fs::File,
    name: &std::ffi::CStr,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<UnixSymlinkState> {
    use std::os::fd::AsRawFd;

    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixSymlinkState::from_stat(&stat))
}

#[cfg(unix)]
fn unix_stat_ctime_nanoseconds(stat: &libc::stat) -> i64 {
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
fn unix_stat_mtime_nanoseconds(stat: &libc::stat) -> i64 {
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

#[cfg(windows)]
fn read_stable_workspace_symlink(
    root: &Path,
    relative: &Path,
    path_before: &fs::Metadata,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    if !path_before.file_type().is_symlink() || !metadata_is_reparse_point(path_before) {
        return Err(workspace_file_changed_error(relative));
    }
    let root_descriptor = crate::utils::beneath::open_root(root)?;
    checkpoint(FingerprintCheckpoint::AfterSecureRootOpen)?;
    let before = crate::utils::beneath::lstat_beneath(&root_descriptor, relative)?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !before.is_symlink {
        return Err(workspace_file_changed_error(relative));
    }
    let (target, descriptor_metadata) =
        read_windows_workspace_reparse_point(&root_descriptor, relative, checkpoint)?;
    let after = crate::utils::beneath::lstat_beneath(&root_descriptor, relative)?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !raw_lstat_matches(&before, &after)
        || !stable_workspace_metadata_matches(path_before, &descriptor_metadata)
    {
        return Err(workspace_file_changed_error(relative));
    }
    Ok(target)
}

#[cfg(not(any(unix, windows)))]
fn read_stable_workspace_symlink(
    _root: &Path,
    _relative: &Path,
    _path_before: &fs::Metadata,
    _checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure workspace symlink fingerprints are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn read_stable_workspace_symlink_beneath(
    root_descriptor: &fs::File,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let (target, state) = read_workspace_symlink_once(root_descriptor, relative, checkpoint)?;
    let (target_after, state_after) =
        read_workspace_symlink_once(root_descriptor, relative, checkpoint)?;
    if state != state_after || target != target_after {
        return Err(workspace_file_changed_error(relative));
    }
    Ok(target)
}

#[cfg(windows)]
fn read_stable_workspace_symlink_beneath(
    root_descriptor: &fs::File,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let before = crate::utils::beneath::lstat_beneath(root_descriptor, relative)?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !before.is_symlink {
        return Err(workspace_file_changed_error(relative));
    }
    let (target, _) = read_windows_workspace_reparse_point(root_descriptor, relative, checkpoint)?;
    let after = crate::utils::beneath::lstat_beneath(root_descriptor, relative)?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !raw_lstat_matches(&before, &after) {
        return Err(workspace_file_changed_error(relative));
    }
    Ok(target)
}

#[cfg(any(windows, test))]
#[derive(Debug, Eq, PartialEq)]
struct ParsedWindowsReparsePoint {
    tag: u32,
    target: Vec<u16>,
    record: Vec<u8>,
}

#[cfg(windows)]
fn read_windows_workspace_reparse_point(
    root_descriptor: &fs::File,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<(PathBuf, fs::Metadata)> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };

    use windows_sys::Win32::Foundation::HANDLE;

    validate_workspace_file_relative_path(relative)?;
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint symlink has no file name",
        )
    })?;
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let directory = match parent {
        Some(parent) => crate::utils::beneath::open_beneath(root_descriptor, parent)?,
        None => root_descriptor.try_clone()?,
    };
    checkpoint(FingerprintCheckpoint::AfterSecureParentOpen)?;

    let root_path = windows_final_path(root_descriptor.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    let directory_path = windows_final_path(directory.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    if !windows_path_is_beneath(&directory_path, &root_path) {
        return Err(io::Error::other(
            "workspace fingerprint symlink parent escaped the workspace root",
        ));
    }
    let directory_metadata = directory.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    let directory_identity = workspace_file_identity(&directory, &directory_metadata)?;
    checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;

    let file = open_windows_workspace_reparse_point(
        &directory_path,
        name,
        relative,
        &root_path,
        checkpoint,
    )?;
    let metadata = file.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !metadata_is_reparse_point(&metadata) {
        return Err(workspace_file_changed_error(relative));
    }
    let identity = workspace_file_identity(&file, &metadata)?;
    checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;
    let target = windows_read_reparse_value(&file, relative, checkpoint)?;
    let target_after = windows_read_reparse_value(&file, relative, checkpoint)?;
    let metadata_after = file.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if target != target_after || !stable_workspace_metadata_matches(&metadata, &metadata_after) {
        return Err(workspace_file_changed_error(relative));
    }

    // Resolve the leaf again through the still-pinned parent and compare its
    // file identity. This rejects a rename/replacement after the first handle
    // was opened without ever asking a path-based read_link to resolve it.
    let reopened = open_windows_workspace_reparse_point(
        &directory_path,
        name,
        relative,
        &root_path,
        checkpoint,
    )?;
    let reopened_metadata = reopened.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    if !metadata_is_reparse_point(&reopened_metadata) {
        return Err(workspace_file_changed_error(relative));
    }
    let reopened_identity = workspace_file_identity(&reopened, &reopened_metadata)?;
    checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;
    let reopened_target = windows_read_reparse_value(&reopened, relative, checkpoint)?;
    if identity != reopened_identity
        || !stable_workspace_metadata_matches(&metadata, &reopened_metadata)
        || target != reopened_target
    {
        return Err(workspace_file_changed_error(relative));
    }

    let directory_metadata_after = directory.metadata()?;
    checkpoint(FingerprintCheckpoint::AfterSecureDescriptorMetadata)?;
    let directory_identity_after = workspace_file_identity(&directory, &directory_metadata_after)?;
    checkpoint(FingerprintCheckpoint::AfterSecureIdentity)?;
    let root_path_after = windows_final_path(root_descriptor.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    let directory_path_after = windows_final_path(directory.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    if directory_identity != directory_identity_after
        || !stable_workspace_metadata_matches(&directory_metadata, &directory_metadata_after)
        || !windows_paths_equal(&root_path, &root_path_after)
        || !windows_paths_equal(&directory_path, &directory_path_after)
    {
        return Err(workspace_file_changed_error(relative));
    }
    Ok((PathBuf::from(OsString::from_wide(&target.target)), metadata))
}

#[cfg(windows)]
fn open_windows_workspace_reparse_point(
    directory_path: &Path,
    name: &std::ffi::OsStr,
    relative: &Path,
    root_path: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<fs::File> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
    };

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory_path.join(name))?;
    checkpoint(FingerprintCheckpoint::AfterSecureLeafOpen)?;
    let final_path = windows_final_path(file.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    if !windows_path_is_beneath(&final_path, root_path) {
        return Err(io::Error::other(format!(
            "workspace fingerprint symlink '{}' escaped the workspace root",
            relative.display()
        )));
    }
    Ok(file)
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "DeviceIoControl"]
    fn workspace_device_io_control(
        device: windows_sys::Win32::Foundation::HANDLE,
        control_code: u32,
        input_buffer: *const core::ffi::c_void,
        input_size: u32,
        output_buffer: *mut core::ffi::c_void,
        output_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
}

#[cfg(windows)]
fn windows_read_reparse_value(
    file: &fs::File,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<ParsedWindowsReparsePoint> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;

    const FSCTL_GET_REPARSE_POINT: u32 = 589_992;
    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut bytes_returned = 0u32;
    let succeeded = unsafe {
        workspace_device_io_control(
            file.as_raw_handle() as HANDLE,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    checkpoint(FingerprintCheckpoint::AfterSecureSymlinkRead)?;
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let returned = bytes_returned as usize;
    if returned > buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace symlink '{}' returned an oversized reparse record",
                relative.display()
            ),
        ));
    }
    buffer.truncate(returned);
    parse_windows_reparse_record(&buffer, relative)
}

#[cfg(any(windows, test))]
fn parse_windows_reparse_record(
    buffer: &[u8],
    relative: &Path,
) -> io::Result<ParsedWindowsReparsePoint> {
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
    const REPARSE_HEADER_BYTES: usize = 8;
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace symlink '{}' returned an invalid reparse record",
                relative.display()
            ),
        )
    };
    let tag = windows_reparse_u32(buffer, 0).ok_or_else(&invalid)?;
    let data_len = usize::from(windows_reparse_u16(buffer, 4).ok_or_else(&invalid)?);
    let record_len = REPARSE_HEADER_BYTES
        .checked_add(data_len)
        .ok_or_else(&invalid)?;
    let record = buffer.get(..record_len).ok_or_else(&invalid)?;
    let (path_buffer_offset, minimum_data_len) = match tag {
        IO_REPARSE_TAG_SYMLINK => (20usize, 12usize),
        IO_REPARSE_TAG_MOUNT_POINT => (16usize, 8usize),
        _ => return Err(invalid()),
    };
    if data_len < minimum_data_len {
        return Err(invalid());
    }
    let substitute_offset = usize::from(windows_reparse_u16(record, 8).ok_or_else(&invalid)?);
    let substitute_len = usize::from(windows_reparse_u16(record, 10).ok_or_else(&invalid)?);
    let print_offset = usize::from(windows_reparse_u16(record, 12).ok_or_else(&invalid)?);
    let print_len = usize::from(windows_reparse_u16(record, 14).ok_or_else(&invalid)?);
    if substitute_offset % 2 != 0
        || substitute_len % 2 != 0
        || print_offset % 2 != 0
        || print_len % 2 != 0
    {
        return Err(invalid());
    }
    let substitute_start = path_buffer_offset
        .checked_add(substitute_offset)
        .ok_or_else(&invalid)?;
    let substitute_end = substitute_start
        .checked_add(substitute_len)
        .ok_or_else(&invalid)?;
    let substitute_bytes = record
        .get(substitute_start..substitute_end)
        .ok_or_else(&invalid)?;
    let print_start = path_buffer_offset
        .checked_add(print_offset)
        .ok_or_else(&invalid)?;
    let print_end = print_start.checked_add(print_len).ok_or_else(&invalid)?;
    let _ = record.get(print_start..print_end).ok_or_else(&invalid)?;
    let target = substitute_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes(*chunk))
        .collect::<Vec<_>>();
    Ok(ParsedWindowsReparsePoint {
        tag,
        target,
        record: record.to_vec(),
    })
}

#[cfg(any(windows, test))]
fn windows_reparse_u16(buffer: &[u8], offset: usize) -> Option<u16> {
    let bytes = buffer.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[cfg(any(windows, test))]
fn windows_reparse_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(not(any(unix, windows)))]
fn read_stable_workspace_symlink_beneath(
    _root_descriptor: &fs::File,
    _relative: &Path,
    _checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure workspace symlink fingerprints are unsupported on this platform",
    ))
}

fn raw_lstat_matches(
    left: &crate::utils::beneath::RawLstat,
    right: &crate::utils::beneath::RawLstat,
) -> bool {
    left.is_symlink == right.is_symlink
        && left.is_dir == right.is_dir
        && left.is_file == right.is_file
        && left.len == right.len
        && left.mode == right.mode
        && left.ctime_sec == right.ctime_sec
        && left.ctime_nsec == right.ctime_nsec
        && left.mtime_sec == right.mtime_sec
        && left.mtime_nsec == right.mtime_nsec
}

fn update_raw_lstat_digest(digest: &mut Sha256, metadata: &crate::utils::beneath::RawLstat) {
    digest.update(metadata.len.to_le_bytes());
    digest.update(metadata.mode.to_le_bytes());
    digest.update(metadata.ctime_sec.to_le_bytes());
    digest.update(metadata.ctime_nsec.to_le_bytes());
    digest.update(metadata.mtime_sec.to_le_bytes());
    digest.update(metadata.mtime_nsec.to_le_bytes());
}

fn ensure_regular_workspace_file(metadata: &fs::Metadata, relative: &Path) -> io::Result<()> {
    if metadata.file_type().is_file() && !metadata_is_reparse_point(metadata) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "workspace entry '{}' is not a regular file or was replaced by a link",
            relative.display()
        ),
    ))
}

fn workspace_file_changed_error(relative: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "workspace file '{}' changed while its Phase 1 fingerprint was captured",
            relative.display()
        ),
    )
}

fn validate_workspace_file_relative_path(relative: &Path) -> io::Result<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace fingerprint path '{}' is not a normal relative file path",
                relative.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_workspace_regular_file_descriptor(
    root: &Path,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    validate_workspace_file_relative_path(relative)?;
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint path has no file name",
        )
    })?;
    let root_descriptor = crate::utils::beneath::open_root(root)?;
    checkpoint(FingerprintCheckpoint::AfterSecureRootOpen)?;
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let directory = match parent {
        Some(parent) => {
            let directory = crate::utils::beneath::open_beneath(&root_descriptor, parent)?;
            checkpoint(FingerprintCheckpoint::AfterSecureParentOpen)?;
            directory
        }
        None => root_descriptor,
    };
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint path contains an interior NUL byte",
        )
    })?;
    // O_NOFOLLOW rejects a swapped symlink leaf, O_NONBLOCK prevents a
    // swapped FIFO/device from stalling before fstat can reject it, and the
    // parent directory descriptor keeps every intermediate component beneath
    // the already-opened workspace root.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        checkpoint(FingerprintCheckpoint::AfterSecureLeafOpen)?;
        return Err(io::Error::new(
            error.kind(),
            format!(
                "failed to open workspace file '{}' without following links: {error}",
                relative.display()
            ),
        ));
    }
    // SAFETY: `openat` returned a fresh owned descriptor, checked nonnegative
    // above, and this is the only `File` constructed from it.
    let file = unsafe { fs::File::from_raw_fd(descriptor) };
    checkpoint(FingerprintCheckpoint::AfterSecureLeafOpen)?;
    Ok(file)
}

#[cfg(windows)]
fn open_workspace_regular_file_descriptor(
    root: &Path,
    relative: &Path,
    checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<fs::File> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};

    use windows_sys::Win32::{
        Foundation::HANDLE, Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
    };

    validate_workspace_file_relative_path(relative)?;
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace fingerprint path has no file name",
        )
    })?;
    let root_descriptor = crate::utils::beneath::open_root(root)?;
    checkpoint(FingerprintCheckpoint::AfterSecureRootOpen)?;
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let directory = match parent {
        Some(parent) => crate::utils::beneath::open_beneath(&root_descriptor, parent)?,
        None => root_descriptor.try_clone()?,
    };
    checkpoint(FingerprintCheckpoint::AfterSecureParentOpen)?;
    let root_path = windows_final_path(root_descriptor.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    let directory_path = windows_final_path(directory.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    if !windows_path_is_beneath(&directory_path, &root_path) {
        return Err(io::Error::other(
            "workspace fingerprint parent escaped the workspace root",
        ));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory_path.join(name))?;
    checkpoint(FingerprintCheckpoint::AfterSecureLeafOpen)?;
    let final_path = windows_final_path(file.as_raw_handle() as HANDLE)?;
    checkpoint(FingerprintCheckpoint::AfterSecureFinalPath)?;
    if !windows_path_is_beneath(&final_path, &root_path) {
        return Err(io::Error::other(
            "workspace fingerprint file escaped the workspace root",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_regular_file_descriptor(
    _root: &Path,
    _relative: &Path,
    _checkpoint: &mut dyn FnMut(FingerprintCheckpoint) -> io::Result<()>,
) -> io::Result<fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure workspace fingerprint reads are unsupported on this platform",
    ))
}

fn stable_workspace_metadata_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type().is_file() == right.file_type().is_file()
        && left.file_type().is_dir() == right.file_type().is_dir()
        && left.file_type().is_symlink() == right.file_type().is_symlink()
        && left.len() == right.len()
        && left.permissions().readonly() == right.permissions().readonly()
        && left.modified().ok() == right.modified().ok()
        && stable_platform_metadata_matches(left, right)
}

#[cfg(unix)]
fn stable_platform_metadata_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(windows)]
fn stable_platform_metadata_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    left.file_attributes() == right.file_attributes()
        && left.creation_time() == right.creation_time()
        && left.last_write_time() == right.last_write_time()
        && left.file_size() == right.file_size()
}

#[cfg(not(any(unix, windows)))]
fn stable_platform_metadata_matches(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn workspace_file_identity(
    _file: &fs::File,
    metadata: &fs::Metadata,
) -> io::Result<WorkspaceFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    Ok(WorkspaceFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn workspace_file_identity(
    file: &fs::File,
    _metadata: &fs::Metadata,
) -> io::Result<WorkspaceFileIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(WorkspaceFileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
fn workspace_file_identity(
    _file: &fs::File,
    _metadata: &fs::Metadata,
) -> io::Result<WorkspaceFileIdentity> {
    Ok(WorkspaceFileIdentity {})
}

#[cfg(windows)]
fn windows_final_path(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_DOS};

    let mut buffer = vec![0u16; 1024];
    let mut length = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            VOLUME_NAME_DOS,
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= buffer.len() {
        buffer.resize(length as usize + 1, 0);
        length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_DOS,
            )
        };
        if length == 0 || length as usize >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

#[cfg(windows)]
fn windows_path_is_beneath(candidate: &Path, root: &Path) -> bool {
    let candidate = candidate.to_string_lossy();
    let root = root.to_string_lossy();
    if candidate.eq_ignore_ascii_case(&root) {
        return true;
    }
    candidate.len() > root.len()
        && candidate
            .as_bytes()
            .get(..root.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root.as_bytes()))
        && matches!(candidate.as_bytes().get(root.len()), Some(b'\\' | b'/'))
}

#[cfg(windows)]
fn windows_paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn fingerprint_walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            let is_dir = entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir());
            entry.depth() == 0 || !ignored_workspace_entry(entry.path(), is_dir)
        });
    builder
}

/// Stream the ignore-aware walk without sorting inside `WalkDir`, reject
/// over-budget workspaces, then sort the bounded manifest deterministically.
/// This prevents one extremely wide directory from being collected by the
/// walker's `sort_by_file_path` before the entry cap can run.
#[derive(Debug)]
struct FingerprintWalkEntry {
    path: PathBuf,
    is_dir: bool,
}

fn collect_fingerprint_paths<C: FingerprintClock>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
) -> io::Result<Vec<PathBuf>> {
    let mut walker = fingerprint_walk_builder(root).build().map(|entry| {
        let entry = entry.map_err(ignore_error_to_io)?;
        let file_type = entry.file_type().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "workspace entry type is unavailable for '{}'",
                    entry.path().display()
                ),
            )
        })?;
        Ok(FingerprintWalkEntry {
            path: entry.path().to_path_buf(),
            is_dir: file_type.is_dir(),
        })
    });
    collect_fingerprint_paths_from_entries(root, budget, started_at, clock, &mut walker)
}

fn collect_fingerprint_paths_from_entries<C, I>(
    root: &Path,
    budget: FingerprintBudget,
    started_at: Instant,
    clock: &C,
    walker: &mut I,
) -> io::Result<Vec<PathBuf>>
where
    C: FingerprintClock,
    I: Iterator<Item = io::Result<FingerprintWalkEntry>>,
{
    let mut paths = Vec::new();
    let mut traversed_entries = 0usize;
    let mut path_bytes = 0usize;
    loop {
        enforce_fingerprint_budget(
            started_at,
            traversed_entries,
            budget,
            clock,
            FingerprintCheckpoint::General,
        )?;
        let next = walker.next();
        enforce_fingerprint_budget(
            started_at,
            traversed_entries,
            budget,
            clock,
            if next.is_none() {
                FingerprintCheckpoint::AfterWalkEof
            } else {
                FingerprintCheckpoint::AfterWalkStep
            },
        )?;
        let Some(entry) = next else {
            break;
        };
        let entry = entry?;
        let path = &entry.path;
        if path == root {
            continue;
        }
        traversed_entries = traversed_entries.saturating_add(1);
        enforce_fingerprint_budget(
            started_at,
            traversed_entries,
            budget,
            clock,
            FingerprintCheckpoint::General,
        )?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| io::Error::other(error.to_string()))?;
        path_bytes = path_bytes.saturating_add(relative.as_os_str().as_encoded_bytes().len());
        enforce_fingerprint_path_budget(path_bytes, budget)?;
        if !entry.is_dir {
            paths.push(relative.to_path_buf());
        }
    }
    enforce_fingerprint_budget(
        started_at,
        traversed_entries,
        budget,
        clock,
        FingerprintCheckpoint::BeforeManifestReturn,
    )?;
    enforce_fingerprint_path_budget(path_bytes, budget)?;
    paths.sort();
    enforce_fingerprint_budget(
        started_at,
        traversed_entries,
        budget,
        clock,
        FingerprintCheckpoint::AfterSort,
    )?;
    Ok(paths)
}

fn enforce_fingerprint_budget<C: FingerprintClock>(
    started_at: Instant,
    entries: usize,
    budget: FingerprintBudget,
    clock: &C,
    checkpoint: FingerprintCheckpoint,
) -> io::Result<()> {
    if entries > budget.max_entries {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace exceeds the {}-entry Phase 1 fingerprint limit",
                budget.max_entries
            ),
        ));
    }
    if clock.elapsed(started_at, checkpoint) > budget.max_duration {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "workspace Phase 1 fingerprint exceeded the {}-second cooperative work budget",
                budget.max_duration.as_secs()
            ),
        ));
    }
    Ok(())
}

fn enforce_fingerprint_path_budget(path_bytes: usize, budget: FingerprintBudget) -> io::Result<()> {
    if path_bytes > budget.max_path_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace path names exceed the {}-byte Phase 1 fingerprint memory budget",
                budget.max_path_bytes
            ),
        ));
    }
    Ok(())
}

fn update_digest_field(digest: &mut Sha256, field: &[u8]) {
    digest.update((field.len() as u64).to_le_bytes());
    digest.update(field);
}

#[cfg(test)]
pub(crate) fn snapshot_workspace_with_contents(root: &Path) -> io::Result<WorkspaceSnapshot> {
    snapshot_workspace_inner(root, true)
}

fn snapshot_workspace_inner(
    root: &Path,
    capture_file_contents: bool,
) -> io::Result<WorkspaceSnapshot> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(|entry| {
            let is_dir = entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir());
            entry.depth() == 0 || !ignored_workspace_entry(entry.path(), is_dir)
        });

    let mut entries = BTreeMap::new();
    let mut file_contents = BTreeMap::new();
    let mut captured_content_bytes = 0usize;
    for entry in builder.build() {
        let entry = entry.map_err(ignore_error_to_io)?;
        let path = entry.path();
        if path == root {
            continue;
        }

        let file_type = if let Some(file_type) = entry.file_type() {
            file_type
        } else {
            fs::symlink_metadata(path)?.file_type()
        };
        if file_type.is_dir() {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .map_err(|err| io::Error::other(err.to_string()))?
            .to_path_buf();
        let (snapshot_entry, contents) = snapshot_entry(path, &file_type, capture_file_contents)?;
        if let Some(contents) = contents {
            let next_total = captured_content_bytes.saturating_add(contents.len());
            if next_total <= SNAPSHOT_CONTENT_MAX_TOTAL_BYTES {
                captured_content_bytes = next_total;
                file_contents.insert(rel.clone(), contents);
            }
        }
        entries.insert(rel, snapshot_entry);
    }

    Ok(WorkspaceSnapshot {
        entries,
        file_contents,
    })
}

pub(crate) fn changed_paths_since_baseline(
    baseline: &WorkspaceSnapshot,
    current: &WorkspaceSnapshot,
) -> Vec<PathBuf> {
    let paths = baseline
        .entries
        .keys()
        .chain(current.entries.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    paths
        .into_iter()
        .filter(|path| baseline.entries.get(path) != current.entries.get(path))
        .collect()
}

pub(crate) fn workspace_entry_if_exists(path: &Path) -> io::Result<Option<WorkspaceEntry>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            snapshot_entry(path, &metadata.file_type(), false).map(|(entry, _)| Some(entry))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn ignored_workspace_entry(path: &Path, is_dir: bool) -> bool {
    protected_workspace_entry(path)
        || generated_artifacts::is_generated_build_dir_path(path)
        || workspace_cache_entry(path, is_dir)
}

fn protected_workspace_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | ".libra" | ".codex" | ".agents"))
}

fn workspace_cache_entry(path: &Path, is_dir: bool) -> bool {
    is_dir && (is_cargo_cache_path(path) || path.join("CACHEDIR.TAG").is_file())
}

pub(crate) fn is_cargo_cache_path(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(|component| component.to_ascii_lowercase())
        .collect::<Vec<_>>();

    components.windows(2).any(|window| {
        matches!(
            window,
            [home, registry]
                if matches!(home.as_str(), "cargo-home" | ".cargo")
                    && registry == "registry"
        )
    })
}

fn snapshot_entry(
    path: &Path,
    file_type: &fs::FileType,
    capture_file_contents: bool,
) -> io::Result<(WorkspaceEntry, Option<Vec<u8>>)> {
    if file_type.is_symlink() {
        return Ok((WorkspaceEntry::Symlink(fs::read_link(path)?), None));
    }

    // Workspace snapshots are used only for change detection between two local
    // filesystem states. They should not depend on repository-scoped LFS or
    // attribute resolution, because isolated task workspaces and tests may run
    // outside a Libra repository context.
    let contents = fs::read(path)?;
    let captured = should_capture_snapshot_file_contents(capture_file_contents, &contents)
        .then(|| contents.clone());
    let entry = WorkspaceEntry::File(Blob::from_content_bytes(contents).id);
    Ok((entry, captured))
}

fn should_capture_snapshot_file_contents(capture_file_contents: bool, contents: &[u8]) -> bool {
    capture_file_contents
        && contents.len() <= SNAPSHOT_CONTENT_MAX_FILE_BYTES
        && std::str::from_utf8(contents).is_ok()
}

fn ignore_error_to_io(err: ignore::Error) -> io::Error {
    let err_text = err.to_string();
    err.into_io_error()
        .unwrap_or_else(|| io::Error::other(err_text))
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs, io,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::{
        FingerprintBudget, FingerprintCheckpoint, FingerprintClock, FingerprintWalkEntry,
        SNAPSHOT_CONTENT_MAX_FILE_BYTES, SystemFingerprintClock, WorkspaceEntry,
        collect_fingerprint_paths, collect_fingerprint_paths_from_entries,
        fingerprint_walk_builder, parse_windows_reparse_record, snapshot_workspace,
        snapshot_workspace_with_contents, workspace_snapshot_fingerprint,
        workspace_snapshot_fingerprint_with_budget,
        workspace_snapshot_fingerprint_with_budget_clock,
        workspace_snapshot_fingerprint_with_budget_clock_and_open_hook,
        workspace_snapshot_metadata_fingerprint,
        workspace_snapshot_metadata_fingerprint_with_budget,
        workspace_snapshot_metadata_fingerprint_with_budget_clock,
        workspace_snapshot_metadata_fingerprint_with_budget_clock_and_lstat_hook,
        workspace_snapshot_stable_fingerprints_with_post_content_hook,
    };

    struct CheckpointFingerprintClock {
        checks: Cell<usize>,
        expire_at: FingerprintCheckpoint,
    }

    impl CheckpointFingerprintClock {
        fn expiring_at(expire_at: FingerprintCheckpoint) -> Self {
            Self {
                checks: Cell::new(0),
                expire_at,
            }
        }
    }

    struct NthCheckpointFingerprintClock {
        matches: Cell<usize>,
        expire_at: FingerprintCheckpoint,
        occurrence: usize,
    }

    impl NthCheckpointFingerprintClock {
        fn expiring_at(expire_at: FingerprintCheckpoint, occurrence: usize) -> Self {
            Self {
                matches: Cell::new(0),
                expire_at,
                occurrence,
            }
        }
    }

    impl FingerprintClock for NthCheckpointFingerprintClock {
        fn elapsed(&self, _started_at: Instant, checkpoint: FingerprintCheckpoint) -> Duration {
            if checkpoint != self.expire_at {
                return Duration::ZERO;
            }
            let matches = self.matches.get().saturating_add(1);
            self.matches.set(matches);
            if matches == self.occurrence {
                Duration::from_secs(2)
            } else {
                Duration::ZERO
            }
        }
    }

    impl FingerprintClock for CheckpointFingerprintClock {
        fn elapsed(&self, _started_at: Instant, checkpoint: FingerprintCheckpoint) -> Duration {
            let checks = self.checks.get().saturating_add(1);
            self.checks.set(checks);
            if checkpoint == self.expire_at {
                Duration::from_secs(2)
            } else {
                Duration::ZERO
            }
        }
    }

    #[cfg(unix)]
    struct NoFileReadFingerprintClock;

    #[cfg(unix)]
    impl FingerprintClock for NoFileReadFingerprintClock {
        fn elapsed(&self, _started_at: Instant, checkpoint: FingerprintCheckpoint) -> Duration {
            assert!(
                !matches!(
                    checkpoint,
                    FingerprintCheckpoint::AfterFileRead | FingerprintCheckpoint::AfterFileEof
                ),
                "unsafe replacement reached a file read: {checkpoint:?}"
            );
            Duration::ZERO
        }
    }

    #[cfg(unix)]
    fn symlink_path(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    fn synthetic_windows_reparse_record(tag: u32, substitute: &[u16], print: &[u16]) -> Vec<u8> {
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
        let inner_header_len = if tag == IO_REPARSE_TAG_SYMLINK {
            12usize
        } else {
            8usize
        };
        let substitute_len = u16::try_from(substitute.len().saturating_mul(2)).unwrap();
        let print_offset = substitute_len;
        let print_len = u16::try_from(print.len().saturating_mul(2)).unwrap();
        let data_len = u16::try_from(
            inner_header_len
                .saturating_add(usize::from(substitute_len))
                .saturating_add(usize::from(print_len)),
        )
        .unwrap();
        let mut record = Vec::new();
        record.extend_from_slice(&tag.to_le_bytes());
        record.extend_from_slice(&data_len.to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes());
        record.extend_from_slice(&substitute_len.to_le_bytes());
        record.extend_from_slice(&print_offset.to_le_bytes());
        record.extend_from_slice(&print_len.to_le_bytes());
        if tag == IO_REPARSE_TAG_SYMLINK {
            record.extend_from_slice(&0u32.to_le_bytes());
        }
        for unit in substitute.iter().chain(print) {
            record.extend_from_slice(&unit.to_le_bytes());
        }
        record
    }

    #[cfg(windows)]
    fn symlink_path(target: &Path, link: &Path) -> io::Result<()> {
        match fs::metadata(target) {
            Ok(metadata) if metadata.is_dir() => std::os::windows::fs::symlink_dir(target, link),
            _ => std::os::windows::fs::symlink_file(target, link),
        }
    }
    #[test]
    fn snapshot_respects_gitignore_without_git_dir() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("web/node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(root.join(".gitignore"), "target/\nweb/node_modules/\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(root.join("target/debug/app"), "bin\n").unwrap();
        fs::write(root.join("web/node_modules/pkg/index.js"), "export {};\n").unwrap();
        fs::write(root.join(".cargo/config.toml"), "[build]\n").unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("src/lib.rs")));
        assert!(
            snapshot
                .entries
                .contains_key(Path::new(".cargo/config.toml"))
        );
        assert!(!snapshot.entries.contains_key(Path::new("target/debug/app")));
        assert!(
            !snapshot
                .entries
                .contains_key(Path::new("web/node_modules/pkg/index.js"))
        );
    }

    #[test]
    fn snapshot_with_contents_captures_small_text_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), "[dependencies]\n").unwrap();

        let snapshot = snapshot_workspace_with_contents(&root).unwrap();

        assert_eq!(
            snapshot.file_contents.get(Path::new("Cargo.toml")),
            Some(&b"[dependencies]\n".to_vec())
        );
    }

    #[test]
    fn metadata_fingerprint_changes_without_reading_file_bodies() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("README.md");
        fs::write(&path, "before\n").unwrap();
        let before = workspace_snapshot_metadata_fingerprint(&root).unwrap();

        fs::write(&path, "after with a different length\n").unwrap();
        let after = workspace_snapshot_metadata_fingerprint(&root).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn content_fingerprint_detects_same_length_change_with_restored_mtime() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("README.md");
        fs::write(&path, "before\n").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = workspace_snapshot_fingerprint(&root).unwrap();

        fs::write(&path, "after!\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(modified)
                    .set_accessed(accessed),
            )
            .unwrap();
        let after = workspace_snapshot_fingerprint(&root).unwrap();

        assert_ne!(
            before, after,
            "Execute authority must hash content even when length and mtime match"
        );
    }

    #[cfg(unix)]
    #[test]
    fn content_fingerprint_rejects_file_swapped_to_external_symlink_before_read() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("victim.txt");
        let outside = temp.path().join("outside-secret.txt");
        fs::write(&path, "workspace content\n").unwrap();
        fs::write(&outside, "external secret must not be read\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(60),
        };

        let error = workspace_snapshot_fingerprint_with_budget_clock_and_open_hook(
            &root,
            budget,
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, path);
                fs::remove_file(candidate)?;
                symlink_path(&outside, candidate)
            },
        )
        .unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn content_fingerprint_rejects_file_swapped_to_fifo_before_read() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("victim.txt");
        fs::write(&path, "workspace content\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(60),
        };

        let error = workspace_snapshot_fingerprint_with_budget_clock_and_open_hook(
            &root,
            budget,
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, path);
                fs::remove_file(candidate)?;
                let fifo = CString::new(candidate.as_os_str().as_bytes()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "FIFO path contains NUL")
                })?;
                let result = unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
                if result == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn content_fingerprint_rejects_parent_swapped_to_external_symlink_before_read() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        let path = nested.join("victim.txt");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(&path, "workspace content\n").unwrap();
        fs::write(
            outside.join("victim.txt"),
            "external secret must not be read\n",
        )
        .unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(60),
        };

        let error = workspace_snapshot_fingerprint_with_budget_clock_and_open_hook(
            &root,
            budget,
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, path);
                fs::remove_file(candidate)?;
                fs::remove_dir(&nested)?;
                symlink_path(&outside, &nested)
            },
        )
        .unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn content_fingerprint_rejects_symlink_parent_swapped_outside_before_readlink() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        let link = nested.join("link");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink_path(Path::new("inside-target"), &link).unwrap();
        symlink_path(Path::new("outside-secret-target"), &outside.join("link")).unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(60),
        };

        let error = workspace_snapshot_fingerprint_with_budget_clock_and_open_hook(
            &root,
            budget,
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, link);
                fs::remove_file(candidate)?;
                fs::remove_dir(&nested)?;
                symlink_path(&outside, &nested)
            },
        )
        .unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_fingerprint_rejects_symlink_parent_swapped_outside_before_readlink() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        let link = nested.join("link");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink_path(Path::new("inside-target"), &link).unwrap();
        symlink_path(Path::new("outside-secret-target"), &outside.join("link")).unwrap();
        let error = workspace_snapshot_metadata_fingerprint_with_budget_clock_and_lstat_hook(
            &root,
            FingerprintBudget {
                max_entries: usize::MAX,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(60),
            },
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, link);
                fs::remove_file(candidate)?;
                fs::remove_dir(&nested)?;
                symlink_path(&outside, &nested)
            },
        )
        .unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_fingerprint_reuses_pinned_root_after_workspace_path_replacement() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let original_root = temp.path().join("original-root");
        fs::create_dir_all(&root).unwrap();
        symlink_path(Path::new("inside-target"), &root.join("link")).unwrap();
        let expected = workspace_snapshot_metadata_fingerprint(&root).unwrap();

        let actual = workspace_snapshot_metadata_fingerprint_with_budget_clock_and_lstat_hook(
            &root,
            FingerprintBudget {
                max_entries: usize::MAX,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(60),
            },
            Instant::now(),
            &NoFileReadFingerprintClock,
            |candidate| {
                assert_eq!(candidate, root.join("link"));
                fs::rename(&root, &original_root)?;
                fs::create_dir_all(&root)?;
                symlink_path(Path::new("outside-secret-target"), &root.join("link"))
            },
        )
        .unwrap();

        assert_eq!(
            actual, expected,
            "metadata scan must keep using the root descriptor opened before the swap"
        );
    }

    #[test]
    fn windows_reparse_parser_uses_symlink_substitute_name() {
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
        let substitute = r"\??\C:\workspace\target"
            .encode_utf16()
            .collect::<Vec<_>>();
        let print = r"C:\workspace\target".encode_utf16().collect::<Vec<_>>();
        let record = synthetic_windows_reparse_record(IO_REPARSE_TAG_SYMLINK, &substitute, &print);

        let parsed = parse_windows_reparse_record(&record, Path::new("link")).unwrap();

        assert_eq!(parsed.tag, IO_REPARSE_TAG_SYMLINK);
        assert_eq!(parsed.target, substitute);
        assert_eq!(parsed.record, record);
    }

    #[test]
    fn windows_reparse_parser_accepts_mount_point_substitute_name() {
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;
        let substitute = r"\??\C:\workspace\mounted"
            .encode_utf16()
            .collect::<Vec<_>>();
        let record = synthetic_windows_reparse_record(IO_REPARSE_TAG_MOUNT_POINT, &substitute, &[]);

        let parsed = parse_windows_reparse_record(&record, Path::new("junction")).unwrap();

        assert_eq!(parsed.tag, IO_REPARSE_TAG_MOUNT_POINT);
        assert_eq!(parsed.target, substitute);
    }

    #[test]
    fn windows_reparse_parser_rejects_truncated_odd_and_out_of_range_records() {
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
        let target = "target".encode_utf16().collect::<Vec<_>>();
        let valid = synthetic_windows_reparse_record(IO_REPARSE_TAG_SYMLINK, &target, &[]);
        let mut truncated = valid.clone();
        truncated.truncate(7);
        let mut declared_too_large = valid.clone();
        declared_too_large[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        let mut odd_offset = valid.clone();
        odd_offset[8..10].copy_from_slice(&1u16.to_le_bytes());
        let mut odd_length = valid.clone();
        odd_length[10..12].copy_from_slice(&1u16.to_le_bytes());
        let mut out_of_range = valid;
        out_of_range[8..10].copy_from_slice(&65_534u16.to_le_bytes());

        for malformed in [
            truncated,
            declared_too_large,
            odd_offset,
            odd_length,
            out_of_range,
        ] {
            let error = parse_windows_reparse_record(&malformed, Path::new("link")).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn fingerprint_budget_counts_directories_and_propagates_entry_limit() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/file.txt"), "content\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: 1,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(60),
        };

        for error in [
            workspace_snapshot_fingerprint_with_budget(&root, budget).unwrap_err(),
            workspace_snapshot_metadata_fingerprint_with_budget(&root, budget).unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error
                    .to_string()
                    .contains("1-entry Phase 1 fingerprint limit"),
                "unexpected budget error: {error}"
            );
        }
    }

    #[test]
    fn fingerprint_budget_propagates_path_name_limit() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("aa"), "first\n").unwrap();
        fs::write(root.join("bb"), "second\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: 3,
            max_duration: Duration::from_secs(60),
        };

        for error in [
            workspace_snapshot_fingerprint_with_budget(&root, budget).unwrap_err(),
            workspace_snapshot_metadata_fingerprint_with_budget(&root, budget).unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error
                    .to_string()
                    .contains("3-byte Phase 1 fingerprint memory budget"),
                "unexpected path budget error: {error}"
            );
        }
    }

    #[test]
    fn fingerprint_budget_timeout_is_typed_and_actionable() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for error in [
            workspace_snapshot_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(FingerprintCheckpoint::AfterWalkEof),
            )
            .unwrap_err(),
            workspace_snapshot_metadata_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(FingerprintCheckpoint::AfterWalkEof),
            )
            .unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(error.to_string().contains("cooperative work budget"));
        }
    }

    #[test]
    fn fingerprint_budget_checks_manifest_post_blocking_and_return_boundaries() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for checkpoint in [
            FingerprintCheckpoint::AfterWalkStep,
            FingerprintCheckpoint::AfterWalkEof,
            FingerprintCheckpoint::BeforeManifestReturn,
            FingerprintCheckpoint::AfterSort,
        ] {
            let error = collect_fingerprint_paths(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(checkpoint),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{checkpoint:?}");
        }
    }

    #[test]
    fn fingerprint_budget_checks_exact_blocking_operations_after_return() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), "content\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for checkpoint in [
            FingerprintCheckpoint::AfterEntryMetadata,
            FingerprintCheckpoint::AfterContentPreMetadata,
            FingerprintCheckpoint::AfterFileOpen,
            FingerprintCheckpoint::AfterFileRead,
            FingerprintCheckpoint::AfterFileEof,
            FingerprintCheckpoint::AfterContentPostMetadata,
            FingerprintCheckpoint::BeforeExactReturn,
        ] {
            let error = workspace_snapshot_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(checkpoint),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{checkpoint:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_budget_checks_each_secure_open_and_verification_boundary() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/file.txt"), "content\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for checkpoint in [
            FingerprintCheckpoint::AfterSecureRootOpen,
            FingerprintCheckpoint::AfterSecureParentOpen,
            FingerprintCheckpoint::AfterSecureLeafOpen,
            FingerprintCheckpoint::AfterSecureDescriptorMetadata,
            FingerprintCheckpoint::AfterSecurePathMetadata,
            FingerprintCheckpoint::AfterSecureIdentity,
        ] {
            let error = workspace_snapshot_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(checkpoint),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{checkpoint:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_budget_checks_post_read_and_reopen_boundaries_by_occurrence() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/file.txt"), "content\n").unwrap();
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for (checkpoint, occurrence) in [
            (FingerprintCheckpoint::AfterSecureDescriptorMetadata, 2),
            (FingerprintCheckpoint::AfterSecurePathMetadata, 2),
            (FingerprintCheckpoint::AfterSecureRootOpen, 2),
            (FingerprintCheckpoint::AfterSecureParentOpen, 2),
            (FingerprintCheckpoint::AfterSecureLeafOpen, 2),
            (FingerprintCheckpoint::AfterSecureDescriptorMetadata, 3),
            (FingerprintCheckpoint::AfterSecureIdentity, 2),
        ] {
            let error = workspace_snapshot_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &NthCheckpointFingerprintClock::expiring_at(checkpoint, occurrence),
            )
            .unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::TimedOut,
                "{checkpoint:?} occurrence {occurrence}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_budget_checks_secure_symlink_read_boundary() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        symlink_path(Path::new("target"), &root.join("link")).unwrap();
        let error = workspace_snapshot_fingerprint_with_budget_clock(
            &root,
            FingerprintBudget {
                max_entries: usize::MAX,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(1),
            },
            Instant::now(),
            &CheckpointFingerprintClock::expiring_at(FingerprintCheckpoint::AfterSecureSymlinkRead),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn fingerprint_budget_checks_metadata_and_symlink_operations_after_return() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("target.txt"), "content\n").unwrap();
        if let Err(error) = symlink_path(Path::new("target.txt"), &root.join("link.txt")) {
            #[cfg(windows)]
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create test symlink: {error}");
        }
        let budget = FingerprintBudget {
            max_entries: usize::MAX,
            max_path_bytes: usize::MAX,
            max_duration: Duration::from_secs(1),
        };

        for checkpoint in [
            FingerprintCheckpoint::AfterEntryMetadata,
            FingerprintCheckpoint::AfterSymlinkRead,
            FingerprintCheckpoint::AfterMetadataPostLstat,
            FingerprintCheckpoint::BeforeMetadataReturn,
        ] {
            let error = workspace_snapshot_metadata_fingerprint_with_budget_clock(
                &root,
                budget,
                Instant::now(),
                &CheckpointFingerprintClock::expiring_at(checkpoint),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{checkpoint:?}");
        }

        let exact_symlink_error = workspace_snapshot_fingerprint_with_budget_clock(
            &root,
            budget,
            Instant::now(),
            &CheckpointFingerprintClock::expiring_at(FingerprintCheckpoint::AfterSymlinkRead),
        )
        .unwrap_err();
        assert_eq!(exact_symlink_error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn fingerprint_budget_streams_wide_directory_before_rejecting() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        for index in 0..8 {
            fs::write(root.join(format!("file-{index}.txt")), "content\n").unwrap();
        }
        let error = collect_fingerprint_paths(
            &root,
            FingerprintBudget {
                max_entries: 3,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(60),
            },
            Instant::now(),
            &SystemFingerprintClock,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("3-entry Phase 1 fingerprint limit")
        );
    }

    #[test]
    fn fingerprint_entry_cap_stops_before_poisoned_iterator_tail() {
        struct PoisonedTailWalk {
            root: PathBuf,
            pulls: usize,
        }

        impl Iterator for PoisonedTailWalk {
            type Item = io::Result<FingerprintWalkEntry>;

            fn next(&mut self) -> Option<Self::Item> {
                let index = self.pulls;
                self.pulls = self.pulls.saturating_add(1);
                match index {
                    0 => Some(Ok(FingerprintWalkEntry {
                        path: self.root.clone(),
                        is_dir: true,
                    })),
                    1..=4 => Some(Ok(FingerprintWalkEntry {
                        path: self.root.join(format!("entry-{index}")),
                        is_dir: false,
                    })),
                    _ => panic!("collector pulled the poisoned tail after exceeding its cap"),
                }
            }
        }

        let root = PathBuf::from("/virtual-workspace");
        let mut walker = PoisonedTailWalk {
            root: root.clone(),
            pulls: 0,
        };
        let error = collect_fingerprint_paths_from_entries(
            &root,
            FingerprintBudget {
                max_entries: 3,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(60),
            },
            Instant::now(),
            &SystemFingerprintClock,
            &mut walker,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(walker.pulls, 5, "root plus cap + 1 entries are sufficient");
    }

    #[test]
    fn bounded_manifest_order_matches_legacy_sorted_walk() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        for relative in [
            "a/file.txt",
            "a/nested/deep.txt",
            "a-foo.txt",
            "a0/file.txt",
            "z",
        ] {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, relative).unwrap();
        }

        let mut legacy_builder = fingerprint_walk_builder(&root);
        legacy_builder.sort_by_file_path(|left, right| left.cmp(right));
        let legacy_paths = legacy_builder
            .build()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                entry.path() != root
                    && !entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_dir())
            })
            .map(|entry| entry.path().strip_prefix(&root).unwrap().to_path_buf())
            .collect::<Vec<_>>();
        let bounded_paths = collect_fingerprint_paths(
            &root,
            FingerprintBudget {
                max_entries: usize::MAX,
                max_path_bytes: usize::MAX,
                max_duration: Duration::from_secs(60),
            },
            Instant::now(),
            &SystemFingerprintClock,
        )
        .unwrap();

        assert_eq!(bounded_paths, legacy_paths);
    }

    #[test]
    fn stable_fingerprint_pair_rejects_change_after_content_scan() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("README.md");
        fs::write(&path, "before\n").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();

        let error = workspace_snapshot_stable_fingerprints_with_post_content_hook(&root, || {
            fs::write(&path, "after!\n")?;
            fs::File::options().write(true).open(&path)?.set_times(
                fs::FileTimes::new()
                    .set_modified(modified)
                    .set_accessed(accessed),
            )
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("content and metadata fingerprints were captured")
        );
    }

    #[test]
    fn snapshot_with_contents_skips_binary_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("image.bin"), [0xff, 0xfe, 0xfd]).unwrap();

        let snapshot = snapshot_workspace_with_contents(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("image.bin")));
        assert!(!snapshot.file_contents.contains_key(Path::new("image.bin")));
    }

    #[test]
    fn snapshot_with_contents_skips_large_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("large.txt"),
            vec![b'a'; SNAPSHOT_CONTENT_MAX_FILE_BYTES + 1],
        )
        .unwrap();

        let snapshot = snapshot_workspace_with_contents(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("large.txt")));
        assert!(!snapshot.file_contents.contains_key(Path::new("large.txt")));
    }

    #[test]
    fn snapshot_skips_default_build_outputs_without_gitignore() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(root.join("target/.rustc_info.json"), "{}\n").unwrap();
        fs::write(root.join("target/debug/app"), "compiled\n").unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("Cargo.lock")));
        assert!(
            !snapshot
                .entries
                .contains_key(Path::new("target/.rustc_info.json"))
        );
        assert!(!snapshot.entries.contains_key(Path::new("target/debug/app")));
    }

    #[test]
    fn snapshot_skips_workspace_cargo_cache_dirs_without_gitignore() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("cargo-home/registry/src/index/dep-1.0.0")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(
            root.join("cargo-home/registry/src/index/dep-1.0.0/Cargo.toml"),
            "[package]\nname = \"dep\"\n",
        )
        .unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("src/lib.rs")));
        assert!(!snapshot.entries.contains_key(Path::new(
            "cargo-home/registry/src/index/dep-1.0.0/Cargo.toml"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_skips_generated_build_symlinks_without_gitignore() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        for name in ["target", "build", ".gradle", "bazel-bin"] {
            let external = temp.path().join(format!("external-{name}"));
            fs::create_dir_all(&external).unwrap();
            symlink_path(&external, &root.join(name)).unwrap();
        }

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("Cargo.lock")));
        for name in ["target", "build", ".gradle", "bazel-bin"] {
            assert!(
                !snapshot.entries.contains_key(Path::new(name)),
                "{name} symlink should be ignored"
            );
        }
    }

    #[test]
    fn snapshot_skips_common_compiled_language_build_outputs_without_gitignore() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("rust/target/debug")).unwrap();
        fs::create_dir_all(root.join("java/build/classes")).unwrap();
        fs::create_dir_all(root.join("java/target/classes")).unwrap();
        fs::create_dir_all(root.join("dotnet/bin/Debug")).unwrap();
        fs::create_dir_all(root.join("dotnet/obj")).unwrap();
        fs::create_dir_all(root.join("swift/.build/debug")).unwrap();
        fs::create_dir_all(root.join("zig/.zig-cache")).unwrap();
        fs::create_dir_all(root.join("zig/zig-out/bin")).unwrap();
        fs::create_dir_all(root.join("cpp/cmake-build-debug")).unwrap();
        fs::create_dir_all(root.join("cpp/CMakeFiles/app.dir")).unwrap();
        fs::create_dir_all(root.join("bazel-bin")).unwrap();
        fs::create_dir_all(root.join("bazel-out")).unwrap();
        fs::create_dir_all(root.join("bazel-testlogs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(root.join("src/bin/tool.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("rust/target/debug/app"), "compiled\n").unwrap();
        fs::write(root.join("java/build/classes/App.class"), "compiled\n").unwrap();
        fs::write(root.join("java/target/classes/App.class"), "compiled\n").unwrap();
        fs::write(root.join("dotnet/bin/Debug/app.dll"), "compiled\n").unwrap();
        fs::write(root.join("dotnet/obj/project.assets.json"), "{}\n").unwrap();
        fs::write(root.join("swift/.build/debug/app"), "compiled\n").unwrap();
        fs::write(root.join("zig/.zig-cache/state"), "cache\n").unwrap();
        fs::write(root.join("zig/zig-out/bin/app"), "compiled\n").unwrap();
        fs::write(root.join("cpp/cmake-build-debug/app"), "compiled\n").unwrap();
        fs::write(root.join("cpp/CMakeFiles/app.dir/main.o"), "compiled\n").unwrap();
        fs::write(root.join("bazel-bin/app"), "compiled\n").unwrap();
        fs::write(root.join("bazel-out/state"), "cache\n").unwrap();
        fs::write(root.join("bazel-testlogs/test.log"), "log\n").unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(snapshot.entries.contains_key(Path::new("src/lib.rs")));
        assert!(snapshot.entries.contains_key(Path::new("src/bin/tool.rs")));
        for generated in [
            "rust/target/debug/app",
            "java/build/classes/App.class",
            "java/target/classes/App.class",
            "dotnet/bin/Debug/app.dll",
            "dotnet/obj/project.assets.json",
            "swift/.build/debug/app",
            "zig/.zig-cache/state",
            "zig/zig-out/bin/app",
            "cpp/cmake-build-debug/app",
            "cpp/CMakeFiles/app.dir/main.o",
            "bazel-bin/app",
            "bazel-out/state",
            "bazel-testlogs/test.log",
        ] {
            assert!(
                !snapshot.entries.contains_key(Path::new(generated)),
                "{generated} should be ignored"
            );
        }
    }

    #[test]
    fn snapshot_skips_protected_metadata_dirs_and_keeps_symlinks() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".libra")).unwrap();
        fs::create_dir_all(root.join(".codex")).unwrap();
        fs::create_dir_all(root.join(".agents")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root.join(".libra/db"), "sqlite\n").unwrap();
        fs::write(root.join(".codex/session"), "state\n").unwrap();
        fs::write(root.join(".agents/cache"), "cache\n").unwrap();
        fs::write(root.join("real.txt"), "hello\n").unwrap();
        symlink_path(Path::new("real.txt"), &root.join("nested/link.txt")).unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert!(!snapshot.entries.contains_key(Path::new(".git/HEAD")));
        assert!(!snapshot.entries.contains_key(Path::new(".libra/db")));
        assert!(!snapshot.entries.contains_key(Path::new(".codex/session")));
        assert!(!snapshot.entries.contains_key(Path::new(".agents/cache")));
        assert_eq!(
            snapshot.entries.get(Path::new("nested/link.txt")),
            Some(&WorkspaceEntry::Symlink(PathBuf::from("real.txt")))
        );
    }
}
