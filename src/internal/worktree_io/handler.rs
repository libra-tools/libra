//! Read-only worktree and object-store request handler.
//!
//! This module owns the actual capability-bound filesystem operations used by
//! the bounded executor. It deliberately depends on utilities and the wire
//! protocol only, so it can be reused by status and other read-only callers.

use std::{
    cell::RefCell,
    io::{self, Write},
    path::Path,
    time::Duration,
};

use super::protocol::{
    CapRequest, CapturedStat, Dirent, FRAME_CAP, IoEvent, IoRequest, ObjectBlobStatus,
    ObjectStoreCapability, ReadDirListing, WireResult, WorktreeRootCapability, bytes_to_path,
    kind_to_u8, path_to_bytes, read_frame, wire_result, write_frame,
};

pub(crate) const WORKER_ARG: &str = "--libra-internal-status-io-worker";
pub(crate) const CAP_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_CAP";
pub(crate) const PPID_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_PPID";

thread_local! {
    /// The helper is a long-lived single-threaded request loop. Cache the
    /// lexical root key and its sealed capability between requests, while
    /// keeping every actual read behind a fresh beneath::open_root call.
    static HELPER_WORKTREE_CAPABILITY:
        RefCell<Option<(Vec<u8>, WorktreeRootCapability)>> = const { RefCell::new(None) };
}

pub(crate) fn handle_request_to_buffer(
    request: IoRequest,
    stdout: &mut Vec<u8>,
) -> io::Result<bool> {
    handle_request(request, stdout)
}
fn seal_worktree_capability(request: &IoRequest) -> io::Result<Option<WorktreeRootCapability>> {
    let root = match request {
        IoRequest::SymlinkMetadata { root, .. }
        | IoRequest::CanonicalizePair { root, .. }
        | IoRequest::ReadDir { root, .. }
        | IoRequest::FileBlobHash { root, .. }
        | IoRequest::MarkerProbe { root, .. } => root,
        IoRequest::ReadObjectBlob { .. } | IoRequest::Shutdown => return Ok(None),
    };
    HELPER_WORKTREE_CAPABILITY.with(|slot| {
        if let Some((cached_root, capability)) = slot.borrow().as_ref()
            && cached_root == root
        {
            return Ok(Some(capability.clone()));
        }
        let capability = WorktreeRootCapability::seal(&bytes_to_path(root))?;
        *slot.borrow_mut() = Some((root.clone(), capability.clone()));
        Ok(Some(capability))
    })
}

fn seal_object_store_capability(request: &IoRequest) -> io::Result<Option<ObjectStoreCapability>> {
    let objects_root = match request {
        IoRequest::ReadObjectBlob { objects_root, .. } => objects_root,
        _ => return Ok(None),
    };
    match ObjectStoreCapability::seal(&bytes_to_path(objects_root)) {
        Ok(capability) => Ok(Some(capability)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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
    let Ok(ppid) = std::env::var(PPID_ENV) else {
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
pub(crate) fn run_worker() -> i32 {
    let expected = match std::env::var(CAP_ENV) {
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

pub(crate) fn handle_request(request: IoRequest, stdout: &mut impl Write) -> io::Result<bool> {
    // Requests can be supplied by the helper's pipe peer, so validate the
    // lexical root and relative paths again after deserialization. Root
    // sealing is intentionally done once below, before the read operation.
    request.validate()?;
    let worktree_capability = seal_worktree_capability(&request)?;
    let object_store_capability = seal_object_store_capability(&request)?;
    match request {
        IoRequest::Shutdown => return Ok(false),
        IoRequest::SymlinkMetadata { path, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let result = lstat_request(&path, capability);
            write_frame(stdout, &IoEvent::DoneStat { result })?;
        }
        IoRequest::CanonicalizePair { left, right, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let left_path = bytes_to_path(&left);
            let right_path = bytes_to_path(&right);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            write_frame(
                stdout,
                &IoEvent::DoneCanonicalize {
                    left: wire_result(
                        capability
                            .resolve(&left_path)
                            .and_then(|path| path.canonicalize())
                            .map(|p| path_to_bytes(&p)),
                    ),
                    right: wire_result(
                        capability
                            .resolve(&right_path)
                            .and_then(|path| path.canonicalize())
                            .map(|p| path_to_bytes(&p)),
                    ),
                },
            )?;
        }
        IoRequest::ReadDir {
            path,
            remaining,
            checkpoint_every,
            ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let listing = read_dir_request(&path, capability, remaining, checkpoint_every, stdout)?;
            write_frame(stdout, &IoEvent::DoneReadDir { listing })?;
        }
        IoRequest::FileBlobHash {
            path,
            hash_kind,
            root_session,
            ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let relative = capability.relative(&path)?;
            let root_fd = crate::utils::beneath::open_root(capability.root())?;
            let hash = match root_session {
                0 => hash_file_blob_beneath(capability.root(), &root_fd, &relative, &hash_kind),
                session => hash_file_blob_beneath_with_session(
                    capability.root(),
                    &root_fd,
                    &relative,
                    &hash_kind,
                    Some(session),
                ),
            };
            let result = match hash {
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
            byte_limit,
            hash_kind,
            ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            maybe_test_slow_object_read(&oid);
            apply_hash_kind(&hash_kind);
            let outcome = match object_store_capability.as_ref() {
                Some(capability) => read_object_blob_request(&oid, capability, byte_limit),
                None => Err(ObjectBlobStatus::Unavailable),
            };
            write_object_blob_outcome(stdout, outcome)?;
        }
        IoRequest::MarkerProbe { dir, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let dir = bytes_to_path(&dir);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let (present, err_kind, err_raw_os) = marker_probe_request(&dir, capability);
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

fn lstat_request(path: &Path, capability: &WorktreeRootCapability) -> WireResult<CapturedStat> {
    let rel = match capability.relative_or_root(path) {
        Ok(rel) => rel,
        Err(error) => {
            return WireResult::Err {
                kind: kind_to_u8(error.kind()),
                raw_os: error.raw_os_error(),
            };
        }
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::lstat_beneath(&fd, &rel))
    {
        Ok(raw) => WireResult::Ok(CapturedStat::from_raw_lstat(&raw)),
        Err(error) => WireResult::Err {
            kind: kind_to_u8(error.kind()),
            raw_os: error.raw_os_error(),
        },
    }
}

fn marker_probe_request(
    dir: &Path,
    capability: &WorktreeRootCapability,
) -> (Option<bool>, Option<u8>, Option<i32>) {
    let rel = match capability.relative(dir) {
        Ok(rel) => rel,
        Err(error) => return (None, Some(kind_to_u8(error.kind())), error.raw_os_error()),
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::marker_present_beneath(&fd, &rel))
    {
        Ok(present) => (Some(present), None, None),
        Err(error) => (None, Some(kind_to_u8(error.kind())), error.raw_os_error()),
    }
}

fn read_dir_request(
    path: &Path,
    capability: &WorktreeRootCapability,
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
    let rel = match capability.relative_or_root(path) {
        Ok(rel) => rel,
        Err(error) => {
            listing
                .error_kinds
                .push((kind_to_u8(error.kind()), error.raw_os_error()));
            listing.entries.clear();
            return Ok(listing);
        }
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::open_beneath(&fd, &rel))
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
                &capability.resolve(&rel)?,
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

pub(crate) fn hash_file_blob_beneath(
    root_path: &Path,
    root: &std::fs::File,
    relative: &Path,
    hash_kind: &str,
) -> io::Result<git_internal::hash::ObjectHash> {
    hash_file_blob_beneath_with_session(root_path, root, relative, hash_kind, None)
}

fn hash_file_blob_beneath_with_session(
    root_path: &Path,
    root: &std::fs::File,
    relative: &Path,
    hash_kind: &str,
    root_session: Option<u64>,
) -> io::Result<git_internal::hash::ObjectHash> {
    apply_hash_kind(hash_kind);
    let stat = crate::utils::beneath::lstat_beneath(root, relative)?;
    if stat.is_symlink {
        let target = crate::utils::beneath::read_symlink_beneath(root, relative)?;
        return Ok(git_internal::internal::object::blob::Blob::from_content_bytes(target).id);
    }

    // Open once through the pinned root and hash the descriptor. This open is
    // intentionally also attempted for non-symlink nodes that are not regular
    // files: a FIFO or a FUSE-backed node may block here, and the helper
    // process deadline must be able to reclaim that syscall. `open_file_beneath`
    // still rejects non-regular descriptors after its pinned, no-follow open.
    // Neither the content nor its LFS attributes are rediscovered through a
    // pathname after this point, so a rename/symlink swap cannot redirect the
    // read.
    let file = crate::utils::beneath::open_file_beneath(root, relative)?;
    let length = file.metadata()?.len();
    if crate::utils::attributes::is_lfs_tracked_beneath_session(
        root_path,
        root,
        relative,
        root_session,
    )? {
        let (oid, total) = hash_lfs_file_handle(&file, length)?;
        if total != length || file.metadata()?.len() != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LFS file changed while it was being hashed",
            ));
        }
        let pointer = crate::utils::lfs::format_pointer_string(&oid, length);
        return Ok(git_internal::internal::object::blob::Blob::from_content(&pointer).id);
    }
    hash_regular_file_handle(file, length)
}

fn hash_regular_file_handle(
    mut file: std::fs::File,
    length: u64,
) -> io::Result<git_internal::hash::ObjectHash> {
    let mut hasher = git_internal::utils::HashAlgorithm::new();
    hasher.update(b"blob ");
    hasher.update(length.to_string().as_bytes());
    hasher.update(b"\0");
    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "worktree file length overflow")
        })?;
        let read = std::io::Read::read(&mut file, &mut buffer[..requested])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worktree file shrank while it was being hashed",
            ));
        }
        remaining -= read as u64;
        hasher.update(&buffer[..read]);
    }
    if file.metadata()?.len() != length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree file changed while it was being hashed",
        ));
    }
    git_internal::hash::ObjectHash::from_bytes(&hasher.finalize()).map_err(io::Error::other)
}

fn hash_lfs_file_handle(file: &std::fs::File, length: u64) -> io::Result<(String, u64)> {
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    let mut reader = file.try_clone()?;
    let mut remaining = length;
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LFS file length overflow"))?;
        let read = std::io::Read::read(&mut reader, &mut buffer[..requested])?;
        if read == 0 {
            break;
        }
        remaining -= read as u64;
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((hex::encode(hasher.finish().as_ref()), total))
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

pub(crate) fn read_object_blob_request(
    oid: &str,
    object_capability: &ObjectStoreCapability,
    byte_limit: u64,
) -> Result<Vec<u8>, ObjectBlobStatus> {
    use crate::utils::client_storage::{ClientStorage, ObjectReadFailure};

    let Ok(hash) = oid.parse::<git_internal::hash::ObjectHash>() else {
        return Err(ObjectBlobStatus::Failed);
    };
    // Local-only + alternates, no directory creation / remote hydrate
    // (WIO-03 security AC).
    let storage =
        ClientStorage::init_local_existing_with_alternates(object_capability.root().to_path_buf());
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

/// Read an object-store blob in-process for library/test callers. Capability
/// sealing stays in this handler so callers cannot accidentally bypass the
/// local-only, no-hydration object-store boundary.
pub(crate) fn read_object_blob_local(
    oid: &str,
    objects_root: &Path,
    byte_limit: u64,
) -> Result<Vec<u8>, ObjectBlobStatus> {
    let request = IoRequest::ReadObjectBlob {
        oid: oid.to_string(),
        objects_root: path_to_bytes(objects_root),
        byte_limit,
        hash_kind: String::from("sha1"),
    };
    let capability = match seal_object_store_capability(&request) {
        Ok(capability) => capability,
        Err(_) => return Err(ObjectBlobStatus::Unavailable),
    };
    match capability {
        Some(capability) => read_object_blob_request(oid, &capability, byte_limit),
        None => Err(ObjectBlobStatus::Unavailable),
    }
}

pub(crate) fn write_object_blob_outcome(
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
    crate::internal::worktree_io::protocol::write_raw_frame(writer, payload)
}
