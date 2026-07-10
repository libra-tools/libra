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
