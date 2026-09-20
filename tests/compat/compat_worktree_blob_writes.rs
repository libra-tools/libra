//! plan issues/470 FM-02 compat guard: every worktree blob write in the listed
//! modules must go through the shared `utils::worktree_blob` primitive, and the
//! retired fixed-mode helpers must stay retired.
//!
//! The guard pins two things:
//! 1. Each listed module references `worktree_blob::write_worktree_blob` (the
//!    routing contract from ADR-FM-02/03).
//! 2. The old direct-write / fixed-chmod helpers and their call shapes do not
//!    come back. Anything else is an unclassified bypass.
//!
//! Test-only code (`#[cfg(test)]` bodies) may legitimately use `fs::write` for
//! fixtures, so the deny-list names the retired production shapes explicitly
//! instead of banning `fs::write` outright.

use std::{fs, path::PathBuf};

/// Modules migrated by FM-02 (plus FM-01's restore, which shares the guard).
const LISTED_MODULES: &[&str] = &[
    "src/command/cherry_pick.rs",
    "src/command/hydrate.rs",
    "src/command/merge.rs",
    "src/command/rebase.rs",
    "src/command/reset.rs",
    "src/command/restore.rs",
    "src/command/revert.rs",
    "src/command/stash.rs",
];

/// Retired production shapes. Each is a direct blob write or a fixed-mode chmod
/// that FM-01/FM-02 replaced with the primitive.
const FORBIDDEN_SHAPES: &[&str] = &[
    "fs::write(&file_path, content)",
    "fs::write(path, content)",
    "fs::write(&target_path, &blob.data)",
    "fs::write(&full, &blob.data)",
    "util::write_file(&blob.data",
    "apply_worktree_blob_mode",
    "apply_file_mode",
    "set_executable_workdir_mode",
];

#[test]
fn listed_modules_route_worktree_blob_writes_through_the_primitive() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut problems = Vec::new();

    for module in LISTED_MODULES {
        let source = fs::read_to_string(root.join(module))
            .unwrap_or_else(|error| panic!("read {module}: {error}"));
        if !source.contains("write_worktree_blob") {
            problems.push(format!(
                "{module}: module does not use the shared primitive"
            ));
        }
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for shape in FORBIDDEN_SHAPES {
                if line.contains(shape) {
                    problems.push(format!("{module}:{}: retired shape `{shape}`", index + 1));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "worktree blob writes must go through utils::worktree_blob:\n{}",
        problems.join("\n")
    );
}
