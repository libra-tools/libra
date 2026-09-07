//! Bounded, read-only worktree and local object-store I/O protocol.

pub mod executor;
pub(crate) mod handler;
pub mod protocol;
pub(crate) mod session;

/// Construct the standard bounded read-only executor without requiring a
/// command-layer handler. This keeps the executor and its capability-bound
/// operations reusable by any internal read-only caller.
pub(crate) fn default_worktree_io() -> executor::WorktreeIo {
    executor::WorktreeIo::default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::protocol::{IoEvent, IoRequest, path_to_bytes, unwrap_wire};

    #[test]
    fn default_factory_executes_a_real_read_only_request() {
        let root = tempfile::tempdir().expect("create worktree root");
        std::fs::write(root.path().join("probe.txt"), b"probe\n").expect("write probe");
        let request = IoRequest::SymlinkMetadata {
            path: path_to_bytes(std::path::Path::new("probe.txt")),
            root: path_to_bytes(root.path()),
        };

        let events = super::default_worktree_io()
            .submit(request, b"probe.txt".to_vec(), Duration::from_secs(1))
            .expect("default factory should execute read-only request");
        let stat = events.into_iter().find_map(|event| match event {
            IoEvent::DoneStat { result } => Some(unwrap_wire(result)),
            _ => None,
        });
        let stat = stat
            .expect("handler should return a terminal stat event")
            .expect("probe file should be readable");
        assert!(stat.is_file);
        assert!(!stat.is_symlink);
    }
}
