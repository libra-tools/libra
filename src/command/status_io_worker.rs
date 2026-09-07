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
    io,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// Hidden argv token. Must be the second argv element; parsed in `main` before CLI.
pub const STATUS_IO_WORKER_ARG: &str = crate::internal::worktree_io::handler::WORKER_ARG;
/// Capability token env. Worker exits 2 if missing or mismatched.
pub const STATUS_IO_WORKER_CAP_ENV: &str = crate::internal::worktree_io::handler::CAP_ENV;
/// Parent pid, so a helper blocked in a syscall can still exit when status dies.
pub const STATUS_IO_WORKER_PPID_ENV: &str = crate::internal::worktree_io::handler::PPID_ENV;

use crate::internal::worktree_io::executor::WorktreeIo;
#[cfg(test)]
pub(crate) use crate::internal::worktree_io::{
    handler::{handle_request, hash_file_blob_beneath, write_object_blob_outcome},
    protocol::{FRAME_CAP, WireResult, kind_to_u8, wire_result, write_frame, write_raw_frame},
    session::{clear as clear_status_io_root_cache, prime as prime_status_io_root_cache},
};
pub(crate) use crate::internal::worktree_io::{
    handler::{read_object_blob_local, run_worker as run_internal_worker},
    protocol::{
        CapturedStat, Dirent, DirentKind, IoEvent, IoRequest, ObjectBlobStatus, ReadDirListing,
        bytes_to_path, dirent_os, io_from_wire, path_to_bytes, unwrap_wire,
    },
    session::{
        begin as begin_status_io_root_session, relative_path as request_worktree_relative,
        root_bytes as request_root_bytes, session_nonce as request_worktree_session,
    },
};

static WORKTREE_IO: OnceLock<WorktreeIo> = OnceLock::new();

fn worktree_io() -> &'static WorktreeIo {
    WORKTREE_IO.get_or_init(crate::internal::worktree_io::default_worktree_io)
}

/// Compatibility entry point retained for `main`; the implementation lives
/// with the reusable worktree I/O handler.
pub fn run_worker() -> i32 {
    run_internal_worker()
}

fn current_hash_kind() -> String {
    match git_internal::hash::get_hash_kind() {
        git_internal::hash::HashKind::Sha256 => "sha256".to_string(),
        _ => "sha1".to_string(),
    }
}

pub(crate) fn deadline_stat(path: &Path) -> Result<io::Result<CapturedStat>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let relative = match request_worktree_relative(&root, path, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::SymlinkMetadata {
                path: relative,
                root,
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
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
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => {
            return Ok((
                Err(error),
                Err(io::Error::other("worktree root unavailable")),
            ));
        }
    };
    let left_relative = match request_worktree_relative(&root, left, true) {
        Ok(relative) => relative,
        Err(error) => {
            let second = io::Error::new(error.kind(), error.to_string());
            return Ok((Err(error), Err(second)));
        }
    };
    let right_relative = match request_worktree_relative(&root, right, true) {
        Ok(relative) => relative,
        Err(error) => {
            let first = io::Error::new(error.kind(), error.to_string());
            return Ok((Err(first), Err(error)));
        }
    };
    let events = worktree_io()
        .submit(
            IoRequest::CanonicalizePair {
                left: left_relative,
                right: right_relative,
                root,
            },
            path_to_bytes(left),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
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
    let relative = match request_worktree_relative(&root, path, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::ReadDir {
                path: relative,
                root,
                remaining,
                checkpoint_every: 32,
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ());
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
    let root = path_to_bytes(workdir);
    let relative = match request_worktree_relative(&root, path, false) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::FileBlobHash {
                path: relative,
                root,
                hash_kind: current_hash_kind(),
                root_session: request_worktree_session(),
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
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
    if !worktree_io().helper_available() {
        let outcome = read_object_blob_local(&oid.to_string(), objects_root, byte_limit);
        return Ok(object_blob_outcome_from_status(outcome));
    }
    let oid_hex = oid.to_string();
    let events = worktree_io()
        .submit_absolute(
            IoRequest::ReadObjectBlob {
                oid: oid_hex.clone(),
                objects_root: path_to_bytes(objects_root),
                byte_limit,
                hash_kind: current_hash_kind(),
            },
            oid_hex.into_bytes(),
            timeout,
        )
        .map_err(|_| ())?;
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
    let relative = match request_worktree_relative(&root, dir, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::MarkerProbe {
                dir: relative,
                root,
            },
            path_to_bytes(dir),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
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
    fn json_frame_rejects_payload_above_cap_without_writing() {
        let event = super::IoEvent::Error {
            message: "x".repeat(super::FRAME_CAP),
        };
        let mut wire = Vec::new();

        let error = super::write_frame(&mut wire, &event).expect_err("frame must be capped");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(wire.is_empty(), "oversized frame must not write a header");
    }

    #[test]
    fn helper_rejects_absolute_worktree_request_paths() {
        let root = tempfile::tempdir().expect("create worktree root");
        let request = super::IoRequest::ReadDir {
            path: super::path_to_bytes(&root.path().join("outside")),
            root: super::path_to_bytes(root.path()),
            remaining: 1,
            checkpoint_every: 1,
        };
        let mut wire = Vec::new();

        let error = super::handle_request(request, &mut wire)
            .expect_err("helper must reject absolute worktree paths");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(wire.is_empty(), "invalid requests must not emit events");
    }

    #[test]
    fn helper_reseals_when_the_wire_root_changes() {
        let root_a = tempfile::tempdir().expect("create first worktree root");
        std::fs::create_dir(root_a.path().join("nested")).expect("first nested directory");
        std::fs::create_dir(root_a.path().join("nested/.libra")).expect("first marker");
        let root_b = tempfile::tempdir().expect("create second worktree root");
        std::fs::create_dir(root_b.path().join("nested")).expect("second nested directory");

        fn probe(root: &std::path::Path) -> Option<bool> {
            let request = super::IoRequest::MarkerProbe {
                dir: b"nested".to_vec(),
                root: super::path_to_bytes(root),
            };
            let mut wire = Vec::new();
            super::handle_request(request, &mut wire).expect("marker probe");
            let events = crate::internal::worktree_io::protocol::parse_event_frames(&wire)
                .expect("marker probe frames");
            events.into_iter().find_map(|event| match event {
                super::IoEvent::DoneMarker { present, .. } => present,
                _ => None,
            })
        }

        assert_eq!(probe(root_a.path()), Some(true));
        assert_eq!(probe(root_b.path()), Some(false));
    }

    #[test]
    fn parent_root_session_validation_is_lexical_for_missing_root() {
        let parent = tempfile::tempdir().expect("create parent directory");
        let missing_root = parent.path().join("missing-worktree-root");
        super::clear_status_io_root_cache();
        super::prime_status_io_root_cache(&missing_root);
        let root = super::path_to_bytes(&missing_root);

        let nested_absolute = missing_root.join("nested");
        let nested = super::request_worktree_relative(&root, &nested_absolute, false)
            .expect("absolute path beneath a missing root remains lexical");
        assert_eq!(
            super::bytes_to_path(&nested),
            std::path::Path::new("nested")
        );

        let nested = super::request_worktree_relative(&root, std::path::Path::new("nested"), false)
            .expect("relative path beneath a missing root remains lexical");
        assert_eq!(
            super::bytes_to_path(&nested),
            std::path::Path::new("nested")
        );

        let root_relative = super::request_worktree_relative(&root, &missing_root, true)
            .expect("the missing root itself remains lexical");
        assert!(root_relative.is_empty());
        super::clear_status_io_root_cache();
    }

    #[test]
    fn raw_frame_rejects_payload_above_cap_before_writing_or_reading() {
        let oversized = vec![0u8; super::FRAME_CAP + 1];
        let mut wire = Vec::new();

        let write_error =
            super::write_raw_frame(&mut wire, &oversized).expect_err("raw frame must be capped");
        assert_eq!(write_error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            wire.is_empty(),
            "oversized raw frame must not write a header"
        );
    }

    #[test]
    fn oversized_object_blob_is_reported_without_an_ok_raw_frame() {
        let mut wire = Vec::new();
        super::write_object_blob_outcome(&mut wire, Ok(vec![0u8; super::FRAME_CAP + 1]))
            .expect("oversized object must produce a status event");

        let events = crate::internal::worktree_io::protocol::parse_event_frames(&wire)
            .expect("status event must remain framed");
        assert!(matches!(
            events.as_slice(),
            [super::IoEvent::DoneObjectBlob {
                status: super::ObjectBlobStatus::TooLarge,
                bytes: None,
            }]
        ));
    }

    #[test]
    fn wire_errors_preserve_kind_and_raw_os_error() {
        let kind_only = super::wire_result::<()>(Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        )));
        let super::WireResult::Err { kind, raw_os } = &kind_only else {
            panic!("permission error must be encoded as an error");
        };
        assert_eq!(*kind, 1);
        assert_eq!(*raw_os, None);
        let decoded = super::unwrap_wire(kind_only).expect_err("wire error must decode");
        assert_eq!(decoded.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(decoded.raw_os_error(), None);

        let source = std::io::Error::from_raw_os_error(2);
        let source_kind = source.kind();
        let with_raw = super::wire_result::<()>(Err(source));
        let super::WireResult::Err { kind, raw_os } = &with_raw else {
            panic!("raw OS error must be encoded as an error");
        };
        assert_eq!(*kind, super::kind_to_u8(source_kind));
        assert_eq!(*raw_os, Some(2));
        let decoded = super::unwrap_wire(with_raw).expect_err("wire error must decode");
        assert_eq!(decoded.kind(), source_kind);
        assert_eq!(decoded.raw_os_error(), Some(2));
    }

    #[test]
    fn absolute_deadline_cancels_job_without_leaking_pending_entry() {
        let path_key = b"unit-test-expired-status-io-job".to_vec();
        let outcome = super::worktree_io().submit_absolute(
            super::IoRequest::Shutdown,
            path_key.clone(),
            std::time::Duration::ZERO,
        );

        assert!(matches!(
            outcome,
            Err(crate::internal::worktree_io::executor::ExecutorError::DeadlineExpired)
        ));
        let _ = path_key;
    }

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
        use std::{ffi::OsString, path::Path};

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
            path: super::path_to_bytes(Path::new("tracked.bin")),
            root: super::path_to_bytes(repo_b.path()),
            hash_kind: "sha1".to_string(),
            root_session: 0,
        };
        let mut buf = Vec::new();
        super::handle_request(request, &mut buf).expect("handle_request");

        let events =
            crate::internal::worktree_io::protocol::parse_event_frames(&buf).expect("frames");
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

    #[cfg(unix)]
    #[test]
    fn pinned_regular_hash_does_not_reopen_replaced_root_path() {
        use git_internal::internal::object::blob::Blob;

        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("root");
        let retired = parent.path().join("retired");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join("tracked.txt"), b"inside\n").expect("inside file");
        std::fs::write(outside.path().join("tracked.txt"), b"outside\n").expect("outside file");
        let root_fd = crate::utils::beneath::open_root(&root).expect("pin root");

        std::fs::rename(&root, &retired).expect("retire original root");
        std::os::unix::fs::symlink(outside.path(), &root).expect("replace root with symlink");
        let hash = super::hash_file_blob_beneath(
            &root,
            &root_fd,
            std::path::Path::new("tracked.txt"),
            "sha1",
        )
        .expect("hash pinned file");
        let expected = Blob::from_content_bytes(b"inside\n".to_vec()).id;
        let outside_hash = Blob::from_content_bytes(b"outside\n".to_vec()).id;
        assert_eq!(hash, expected, "hash must come from the pinned root file");
        assert_ne!(hash, outside_hash, "hash must not follow replacement root");

        std::fs::remove_file(&root).expect("remove replacement symlink");
    }

    #[cfg(unix)]
    #[test]
    fn pinned_symlink_hash_reads_leaf_target_relative_to_root() {
        use git_internal::internal::object::blob::Blob;

        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join("target.txt"), b"target content\n").expect("target");
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).expect("symlink");
        let root_fd = crate::utils::beneath::open_root(&root).expect("pin root");

        let hash = super::hash_file_blob_beneath(
            &root,
            &root_fd,
            std::path::Path::new("link.txt"),
            "sha1",
        )
        .expect("hash symlink leaf");
        let expected = Blob::from_content_bytes(b"target.txt".to_vec()).id;
        assert_eq!(
            hash, expected,
            "Git symlink blob uses the link target bytes"
        );
    }

    #[test]
    fn pinned_lfs_hash_binds_content_and_attributes_to_same_root() {
        use git_internal::internal::object::blob::Blob;

        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("root");
        let retired = parent.path().join("retired");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join(".gitattributes"), b"*.bin filter=lfs\n").expect("LFS attrs");
        let original = b"original LFS payload\n";
        std::fs::write(root.join("tracked.bin"), original).expect("original payload");
        let root_fd = crate::utils::beneath::open_root(&root).expect("pin root");

        std::fs::rename(&root, &retired).expect("retire original root");
        std::fs::create_dir(&root).expect("replacement root");
        std::fs::write(root.join(".gitattributes"), b"*.txt filter=lfs\n")
            .expect("replacement attrs");
        std::fs::write(root.join("tracked.bin"), b"replacement payload\n")
            .expect("replacement payload");

        let hash = super::hash_file_blob_beneath(
            &root,
            &root_fd,
            std::path::Path::new("tracked.bin"),
            "sha1",
        )
        .expect("hash pinned LFS file");
        let oid = crate::utils::lfs::calc_lfs_file_hash(retired.join("tracked.bin"))
            .expect("original LFS oid");
        let pointer = crate::utils::lfs::format_pointer_string(&oid, original.len() as u64);
        let expected = Blob::from_content(&pointer).id;
        assert_eq!(
            hash, expected,
            "LFS decision and bytes must use pinned root"
        );
    }
}
