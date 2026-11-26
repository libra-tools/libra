//! Shared status observation DTOs (`Changes`, `WorktreeStamp`, `ObservedPath`, `WorktreeStamps`).
//!
//! Sunk into `libra-core` during `WSM-02B` so `libra-ai` (`WSM-04`) and `src/command/status.rs`
//! can share worktree-observation types without a reverse dependency on `crate::command`.

use std::{collections::HashMap, path::PathBuf};

use crate::utils::{
    beneath::{EntryIdentity, RawLstat},
    util,
};

/// path: to workdir
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub new: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    /// Detected renames: (source_path, target_path) pairs.
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.new.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
    }

    /// to relative path(to cur_dir)
    pub fn to_relative(&self) -> Changes {
        let mut change = self.clone();
        [&mut change.new, &mut change.modified, &mut change.deleted]
            .into_iter()
            .for_each(|paths| {
                *paths = paths.iter().map(util::workdir_to_current).collect();
            });
        change.renamed = change
            .renamed
            .into_iter()
            .map(|(old, new)| {
                (
                    util::workdir_to_current(&old),
                    util::workdir_to_current(&new),
                )
            })
            .collect();
        change
    }

    pub fn polymerization(&self) -> Vec<PathBuf> {
        let mut poly = self.new.clone();
        poly.extend(self.modified.clone());
        poly.extend(self.deleted.clone());
        poly.extend(self.renamed.iter().map(|(_, new)| new.clone()));
        poly
    }

    pub fn extend(&mut self, other: Changes) {
        self.new.extend(other.new);
        self.modified.extend(other.modified);
        self.deleted.extend(other.deleted);
        self.renamed.extend(other.renamed);
    }
}

/// A worktree entry's identity, metadata and content observed during status
/// scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeStamp {
    pub identity: EntryIdentity,
    pub stat: RawLstat,
    /// The content digest in the write-fence row format, or `None` when the
    /// path carries no content (a directory) or its content could not be read
    /// within the fence's bounds.
    pub digest: Option<String>,
    /// The bytes the content digest covers, for the fence's byte budget.
    pub bytes: u64,
}

/// What the status scan observed for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedPath {
    /// The path does not exist at the scan.
    Absent,
    /// The path was read, with its identity, metadata and content digest.
    Observed(WorktreeStamp),
    /// The path changed while the scan read it, so the scan cannot describe
    /// it. The observation is never repaired afterwards.
    Unstable,
}

/// Per-path observation of one status scan.
pub type WorktreeStamps = HashMap<PathBuf, ObservedPath>;
