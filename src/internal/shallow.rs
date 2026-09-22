//! Shared shallow-boundary helper for history walks (ADR-CL-05).
//!
//! `.libra/shallow` lists commits whose parents were not fetched. History
//! walks must treat those commits as roots. A missing file means complete
//! history. An unreadable or illegal file is fail-closed: a corrupt boundary
//! list must not be treated as a healthy complete repository (ADR-CL-02).
//!
//! This module is the single parser for the shallow file. Command-layer
//! readers (`fetch::read_shallow_boundaries`, `fsck`, `log`, `rev-list`)
//! must go through it rather than re-implementing the format.

use std::{
    collections::{BTreeSet, HashSet},
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use git_internal::hash::ObjectHash;
use thiserror::Error;

use crate::utils::util;

/// Commits listed in `.libra/shallow`.
#[derive(Debug, Clone, Default)]
pub struct ShallowSet {
    boundaries: HashSet<ObjectHash>,
}

/// Fail-closed shallow-metadata errors.
#[derive(Debug, Error)]
pub enum ShallowError {
    #[error("failed to locate repository storage for shallow metadata: {source}")]
    Locate {
        #[source]
        source: io::Error,
    },
    #[error("failed to read shallow metadata '{}': {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid shallow metadata entry at '{}:{}': {reason}", path.display(), line)]
    InvalidOid {
        path: PathBuf,
        line: usize,
        oid: String,
        reason: String,
    },
}

impl ShallowError {
    /// User-facing recovery hint for `LBR-REPO-002`.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::InvalidOid { .. } => {
                "each line of .libra/shallow must be a full object id; remove the file if the clone is complete"
            }
            Self::Locate { .. } | Self::Read { .. } => {
                "shallow metadata is corrupt; fix or remove .libra/shallow"
            }
        }
    }
}

impl ShallowSet {
    /// Load `.libra/shallow` for the current repository.
    ///
    /// A missing file is an empty set.
    pub fn load() -> Result<Self, ShallowError> {
        load_at(&shallow_file_path()?)
    }

    /// Load an explicit shallow file. A missing file is an empty set.
    pub fn load_at(path: &Path) -> Result<Self, ShallowError> {
        load_at(path)
    }

    /// Empty set: complete history.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether `commit_id` is a shallow-boundary commit.
    pub fn is_boundary(&self, commit_id: &ObjectHash) -> bool {
        self.boundaries.contains(commit_id)
    }

    /// Parents that a history walk may follow.
    ///
    /// A boundary commit is treated as a root: its recorded parents are not
    /// returned. Display paths (`%P`, `--parents`) must keep reading
    /// `commit.parent_commit_ids` so `cat-file -p` and format output stay
    /// faithful to the stored object.
    pub fn parents_for_walk<'a>(
        &self,
        commit_id: &ObjectHash,
        parents: &'a [ObjectHash],
    ) -> &'a [ObjectHash] {
        if self.is_boundary(commit_id) {
            &[]
        } else {
            parents
        }
    }

    /// Boundary object ids (for merge-base / ahead-behind).
    pub fn oids(&self) -> &HashSet<ObjectHash> {
        &self.boundaries
    }

    /// Hex OIDs in sorted order (same shape as the fetch reader).
    pub fn oids_hex(&self) -> BTreeSet<String> {
        self.boundaries.iter().map(ToString::to_string).collect()
    }
}

/// Sorted hex OIDs from the current repository's shallow file.
pub fn boundary_oids() -> Result<BTreeSet<String>, ShallowError> {
    Ok(ShallowSet::load()?.oids_hex())
}

/// Sorted hex OIDs from an explicit shallow file.
pub fn boundary_oids_at(path: &Path) -> Result<BTreeSet<String>, ShallowError> {
    Ok(ShallowSet::load_at(path)?.oids_hex())
}

fn shallow_file_path() -> Result<PathBuf, ShallowError> {
    util::try_get_storage_path(None)
        .map(|storage| storage.join("shallow"))
        .map_err(|source| ShallowError::Locate { source })
}

fn load_at(path: &Path) -> Result<ShallowSet, ShallowError> {
    let path = path.to_path_buf();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ShallowSet::empty());
        }
        Err(source) => {
            return Err(ShallowError::Read { path, source });
        }
    };

    let mut boundaries = HashSet::new();
    for (line_no, line) in content.lines().enumerate() {
        let oid = line.trim();
        if oid.is_empty() {
            continue;
        }
        let hash = ObjectHash::from_str(oid).map_err(|source| ShallowError::InvalidOid {
            path: path.clone(),
            line: line_no + 1,
            oid: oid.to_string(),
            reason: source.to_string(),
        })?;
        boundaries.insert(hash);
    }
    Ok(ShallowSet { boundaries })
}

#[cfg(test)]
mod tests {
    use git_internal::hash::{HashKind, set_hash_kind_for_test};

    use super::*;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PARENT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn parse_oid(hex: &str) -> ObjectHash {
        ObjectHash::from_str(hex).expect("test oid")
    }

    #[test]
    fn load_at_missing_file_is_empty() {
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let set = ShallowSet::load_at(&dir.path().join("shallow")).expect("missing file");
        assert!(set.oids_hex().is_empty());
    }

    #[test]
    fn load_at_accepts_hex_oids_and_skips_blank_lines() {
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("shallow");
        fs::write(&path, format!("{HEAD}\n\n{PARENT}\n")).expect("write shallow");
        let set = ShallowSet::load_at(&path).expect("valid file");
        assert!(set.is_boundary(&parse_oid(HEAD)));
        assert!(set.is_boundary(&parse_oid(PARENT)));
        assert_eq!(set.oids_hex().len(), 2);
    }

    #[test]
    fn load_at_rejects_garbage_fail_closed() {
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("shallow");
        fs::write(&path, "not-an-oid\n").expect("write shallow");
        let error = ShallowSet::load_at(&path).expect_err("garbage must fail");
        assert!(
            matches!(error, ShallowError::InvalidOid { line: 1, .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parents_for_walk_skips_parents_of_boundary_commits() {
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let mut set = ShallowSet::empty();
        let head = parse_oid(HEAD);
        let parent = parse_oid(PARENT);
        set.boundaries.insert(head);
        let recorded = [parent];
        assert!(set.parents_for_walk(&head, &recorded).is_empty());
        assert_eq!(set.parents_for_walk(&parent, &recorded), &recorded);
    }
}
