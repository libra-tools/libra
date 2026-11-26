//! Scan-local opaque gitlink boundaries, queried only through bounded I/O.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    time::Instant,
};

use super::{
    Index, IoEvent, IoRequest, WorkspaceSnapshotter, in_process_test_host, path_to_bytes,
    unwrap_wire,
};
use crate::utils::beneath::{EntryIdentity, EntryIdentityKey, EntryKind};

pub(super) struct GitlinkBoundaries {
    observations: BTreeMap<PathBuf, Option<EntryIdentity>>,
    identities: BTreeMap<EntryIdentityKey, EntryKind>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum BoundaryMatch {
    Visible,
    Opaque,
    Ambiguous,
}

impl GitlinkBoundaries {
    pub(super) fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    pub(super) fn contains_literal(&self, path: &Path) -> bool {
        path.ancestors()
            .any(|ancestor| self.observations.contains_key(ancestor))
    }

    pub(super) fn classify(&self, candidate: EntryIdentity) -> BoundaryMatch {
        use EntryKind::{Directory, File, Other, Symlink};

        match (self.identities.get(&candidate.key), candidate.kind) {
            (None, _) => BoundaryMatch::Visible,
            (Some(Directory), Directory) => BoundaryMatch::Opaque,
            (Some(File | Symlink | Other), _) | (Some(Directory), File | Symlink | Other) => {
                BoundaryMatch::Ambiguous
            }
        }
    }
}

impl WorkspaceSnapshotter {
    pub(super) fn gitlink_boundaries(
        &self,
        index: &Index,
        deadline: Instant,
    ) -> Option<GitlinkBoundaries> {
        // All stages own opaque boundaries, including unmerged and missing
        // gitlinks. Physical lookup also catches case and NFC/NFD aliases.
        let roots: BTreeSet<PathBuf> = (0..=3)
            .flat_map(|stage| index.tracked_entries(stage))
            .filter(|entry| entry.mode & 0o170000 == 0o160000)
            .map(|entry| PathBuf::from(&entry.name))
            .collect();
        let mut identities = BTreeMap::new();
        let mut observations = BTreeMap::new();
        for root in roots {
            let observed = match self.entry_identity(&root, deadline) {
                Ok(identity) => {
                    if identities
                        .insert(identity.key, identity.kind)
                        .is_some_and(|previous| previous != identity.kind)
                    {
                        return None;
                    }
                    Some(identity)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_) => return None,
            };
            observations.insert(root, observed);
        }
        Some(GitlinkBoundaries {
            observations,
            identities,
        })
    }

    pub(super) fn gitlink_boundaries_unchanged(
        &self,
        boundaries: &GitlinkBoundaries,
        deadline: Instant,
    ) -> bool {
        // This detects drift at enumeration boundaries, not an atomic
        // filesystem freeze. Ignore rules can be read during enumeration;
        // continued mutation, ABA and changes after this check remain possible.
        for (root, initial) in &boundaries.observations {
            let current = match self.entry_identity(root, deadline) {
                Ok(identity) => Some(identity),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_) => return false,
            };
            if &current != initial {
                return false;
            }
        }
        true
    }

    pub(super) fn entry_identity(
        &self,
        path: &Path,
        deadline: Instant,
    ) -> io::Result<EntryIdentity> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "entry identity deadline expired",
            ));
        }
        let path_key = path_to_bytes(path);
        let request = IoRequest::EntryIdentity {
            path: path_key.clone(),
            root: path_to_bytes(&self.scope.worktree_root),
        };
        let events = if in_process_test_host() {
            self.io.submit_in_process(request, path_key, timeout)
        } else {
            self.io.submit_absolute(request, path_key, timeout)
        }
        .map_err(|error| io::Error::other(format!("entry identity worker failed: {error}")))?;
        if Instant::now() > deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "entry identity deadline expired",
            ));
        }
        // Progress/heartbeat events cannot establish an identity. An absent
        // terminal result remains unknown and therefore fails closed.
        for event in events {
            if let IoEvent::DoneEntryIdentity { result } = event {
                return unwrap_wire(result);
            }
        }
        Err(io::Error::other(
            "entry identity worker returned no terminal result",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(number: u128, kind: EntryKind) -> EntryIdentity {
        EntryIdentity {
            key: EntryIdentityKey {
                volume: 1,
                file_id: number.to_le_bytes(),
            },
            kind,
        }
    }

    #[test]
    fn distinct_directory_identities_preserve_case_sensitive_siblings() {
        // Given: case-fold-equivalent names refer to different directories.
        let indexed = identity(1, EntryKind::Directory);
        let boundary = GitlinkBoundaries {
            observations: BTreeMap::from([(PathBuf::from("vendor/sub"), Some(indexed))]),
            identities: BTreeMap::from([(indexed.key, indexed.kind)]),
        };

        // When: the real sibling's identity is classified.
        let outcome = boundary.classify(identity(2, EntryKind::Directory));

        // Then: name similarity alone never discards a distinct directory.
        assert!(!boundary.contains_literal(Path::new("vendor/Sub")));
        assert_eq!(outcome, BoundaryMatch::Visible);
        assert!(boundary.contains_literal(Path::new("vendor/sub/inner")));
    }

    #[test]
    fn matching_directory_identities_are_opaque() {
        // Given: one physical directory is named by an indexed gitlink.
        let indexed = identity(1, EntryKind::Directory);
        let boundary = GitlinkBoundaries {
            observations: BTreeMap::from([(PathBuf::from("vendor/sub"), Some(indexed))]),
            identities: BTreeMap::from([(indexed.key, indexed.kind)]),
        };

        // When: an alternative spelling resolves to the same directory.
        let outcome = boundary.classify(indexed);

        // Then: it remains opaque regardless of case or Unicode spelling.
        assert_eq!(outcome, BoundaryMatch::Opaque);
    }

    #[test]
    fn matching_non_directory_identities_remain_ambiguous() {
        for kind in [EntryKind::File, EntryKind::Symlink, EntryKind::Other] {
            // Given: a file-like gitlink placeholder has another name.
            let indexed = identity(1, kind);
            let boundary = GitlinkBoundaries {
                observations: BTreeMap::from([(PathBuf::from("vendor/sub"), Some(indexed))]),
                identities: BTreeMap::from([(indexed.key, indexed.kind)]),
            };

            // When: identity matches but the directory entry is nonliteral.
            let outcome = boundary.classify(indexed);

            // Then: callers must mark an omitted hardlink or alias Partial.
            assert_eq!(outcome, BoundaryMatch::Ambiguous);
        }
    }
}
