//! Deterministic enumeration order; all non-listing requests use the real handler.

use std::{cell::Cell, io, path::Path, time::Instant};

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};

use crate::internal::worktree_io::{
    handler::handle_request_to_buffer,
    protocol::{Dirent, IoEvent, IoRequest, ReadDirListing, bytes_to_path, write_frame},
};

thread_local! {
    static CONSUME_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub(super) fn consume_deadline_in_next_listing(deadline: Instant) {
    CONSUME_DEADLINE.with(|slot| slot.set(Some(deadline)));
}

pub(super) fn blob_oid(bytes: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, bytes)
}

pub(super) fn ordered_listing(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
    if let IoRequest::ReadDir { path, .. } = &request {
        if let Some(deadline) = CONSUME_DEADLINE.with(|slot| slot.take()) {
            // A held, unsignalled channel consumes precisely the caller's remaining
            // budget. Unlike a guessed sleep, the response cannot precede expiry.
            let (_held_sender, receiver) = std::sync::mpsc::channel::<()>();
            while Instant::now() < deadline {
                let _ = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            }
        }
        let directory = bytes_to_path(path);
        let entries: &[(&str, bool)] = if directory.as_os_str().is_empty() {
            &[("safe.txt", false), ("blocked", true)]
        } else if directory == Path::new("blocked") {
            &[("secret.txt", false), ("visible.txt", false)]
        } else {
            &[]
        };
        for (name, is_dir) in entries {
            write_frame(
                output,
                &IoEvent::RecordDirent(Dirent {
                    name: name.as_bytes().to_vec(),
                    is_dir: *is_dir,
                    is_file: !*is_dir,
                    is_symlink: false,
                    type_ok: true,
                }),
            )?;
        }
        write_frame(
            output,
            &IoEvent::DoneReadDir {
                listing: ReadDirListing {
                    entries: Vec::new(),
                    error_kinds: Vec::new(),
                    taken: entries.len(),
                    hit_cap: false,
                    timed_out: false,
                },
            },
        )?;
        return Ok(true);
    }
    if let IoRequest::FileBlobHash { root, .. } = &request {
        std::fs::write(bytes_to_path(root).join(".libra/hash-requested"), b"yes")?;
    }
    handle_request_to_buffer(request, output)
}
