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

    #[test]
    fn default_factory_preserves_entry_identity_for_two_names_of_one_file() {
        use std::path::Path;

        use crate::utils::beneath::EntryKind;

        // Given two hard-linked names for one physical file beneath a sealed root.
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("file"), "content").expect("file");
        std::fs::hard_link(root.path().join("file"), root.path().join("alias")).expect("hard link");
        let executor = super::default_worktree_io();

        // When both lookups use the same bounded I/O protocol as the caller.
        let identities: Vec<_> = ["file", "alias"]
            .into_iter()
            .map(|name| {
                let events = executor
                    .submit_in_process(
                        IoRequest::EntryIdentity {
                            path: path_to_bytes(Path::new(name)),
                            root: path_to_bytes(root.path()),
                        },
                        name.as_bytes().to_vec(),
                        Duration::from_secs(1),
                    )
                    .expect("identity request");
                events
                    .into_iter()
                    .find_map(|event| match event {
                        IoEvent::DoneEntryIdentity { result } => Some(unwrap_wire(result)),
                        _ => None,
                    })
                    .expect("terminal identity")
                    .expect("file identity")
            })
            .collect();

        // Then different lexical names resolve to the same complete physical key.
        assert_eq!(identities[0].key, identities[1].key);
        assert_eq!(identities[0].kind, EntryKind::File);
    }
}
