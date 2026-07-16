use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use git_internal::{hash::ObjectHash, internal::index::Index};

#[derive(Debug, Clone, Copy)]
pub(crate) struct UnmergedStage {
    pub(crate) mode: u32,
    pub(crate) hash: ObjectHash,
}

#[derive(Debug, Clone)]
pub(crate) struct UnmergedEntry {
    pub(crate) path: PathBuf,
    stages: [Option<UnmergedStage>; 3],
}

impl UnmergedEntry {
    pub(crate) fn new(path: PathBuf, stages: [Option<UnmergedStage>; 3]) -> Self {
        Self { path, stages }
    }

    pub(crate) fn stage(&self, stage: u8) -> Option<UnmergedStage> {
        let index = usize::from(stage.checked_sub(1)?);
        self.stages.get(index).copied().flatten()
    }

    pub(crate) fn xy(&self) -> (char, char) {
        match (
            self.stage(1).is_some(),
            self.stage(2).is_some(),
            self.stage(3).is_some(),
        ) {
            (true, false, false) => ('D', 'D'),
            (false, true, false) => ('A', 'U'),
            (true, true, false) => ('U', 'D'),
            (false, false, true) => ('U', 'A'),
            (true, false, true) => ('D', 'U'),
            (false, true, true) => ('A', 'A'),
            (true, true, true) => ('U', 'U'),
            (false, false, false) => ('U', 'U'),
        }
    }

    pub(crate) fn with_path(self, path: PathBuf) -> Self {
        Self { path, ..self }
    }
}

pub(crate) fn collect(index: &Index) -> Vec<UnmergedEntry> {
    let mut by_path: BTreeMap<String, [Option<UnmergedStage>; 3]> = BTreeMap::new();
    for stage in 1..=3 {
        for entry in index.tracked_entries(stage) {
            by_path.entry(entry.name.clone()).or_insert([None; 3])[usize::from(stage - 1)] =
                Some(UnmergedStage {
                    mode: entry.mode,
                    hash: entry.hash,
                });
        }
    }

    // A stage-0 entry marks the path as resolved: Git treats the merged entry
    // as authoritative, so leftover higher stages must not surface as `u`
    // conflict rows alongside the normal tracked row.
    by_path.retain(|path, _| !index.tracked(path, 0));

    by_path
        .into_iter()
        .map(|(path, stages)| UnmergedEntry::new(PathBuf::from(path), stages))
        .collect()
}

pub(crate) fn path_matches(path: &Path, pathspecs: &[PathBuf]) -> bool {
    pathspecs.is_empty()
        || pathspecs
            .iter()
            .any(|pathspec| path == pathspec || path.starts_with(pathspec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(stages: [bool; 3]) -> UnmergedEntry {
        let hash = ObjectHash::new(&[0u8; 20]);
        let stage = |present: bool| {
            present.then_some(UnmergedStage {
                mode: 0o100644,
                hash,
            })
        };
        UnmergedEntry::new(
            PathBuf::from("conflict.txt"),
            [stage(stages[0]), stage(stages[1]), stage(stages[2])],
        )
    }

    #[test]
    fn xy_covers_all_seven_unmerged_combinations() {
        // Git status porcelain v1 unmerged XY codes (git-status(1)).
        assert_eq!(entry([true, false, false]).xy(), ('D', 'D')); // both deleted
        assert_eq!(entry([false, true, false]).xy(), ('A', 'U')); // added by us
        assert_eq!(entry([true, true, false]).xy(), ('U', 'D')); // deleted by them
        assert_eq!(entry([false, false, true]).xy(), ('U', 'A')); // added by them
        assert_eq!(entry([true, false, true]).xy(), ('D', 'U')); // deleted by us
        assert_eq!(entry([false, true, true]).xy(), ('A', 'A')); // both added
        assert_eq!(entry([true, true, true]).xy(), ('U', 'U')); // both modified
    }

    #[test]
    fn xy_empty_stages_defaults_to_uu() {
        // Defensive: collectors should never emit empty stage sets, but if they
        // do, treat as content conflict rather than inventing a new code.
        assert_eq!(entry([false, false, false]).xy(), ('U', 'U'));
    }
}
