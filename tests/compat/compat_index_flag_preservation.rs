//! plan issues/490 SW-03 compat guard: staging-class index replacements must
//! route through `utils::index_ext::update_preserving_flags`, so the index v3
//! extended flags (`skip_worktree`, `intent_to_add`) survive a replacement.
//!
//! The guard scans the production half of each listed module (up to the first
//! `#[cfg(test)]`) for direct `index.update(` / `index.add(` calls and allows
//! only the classified exceptions below, each of which carries the extended
//! flags itself.

use std::{fs, path::PathBuf};

const STAGING_MODULES: &[&str] = &[
    "src/command/add.rs",
    "src/command/update_index.rs",
    "src/command/restore.rs",
    "src/command/stash.rs",
    "src/command/mv.rs",
    "src/command/remove.rs",
    "src/command/commit.rs",
];

/// Classified direct replacements. Each entry is a `(module, line fragment)`
/// pair and a reason:
/// - `mv.rs` copies `skip_worktree`/`intent_to_add` from the removed source
///   entry onto the destination entry, so calling the helper (which looks up
///   the *destination*) would be wrong.
const ALLOWED: &[(&str, &str)] = &[("src/command/mv.rs", "index.add(entry);")];

/// SW-04 history-rewrite modules whose whole-index rebuild must carry
/// `skip_worktree` over from the previous index.
const REBUILD_MODULES: &[&str] = &[
    "src/command/reset.rs",
    "src/command/revert.rs",
    "src/command/rebase.rs",
];

/// `read-tree` rebuilds (no `-m`) and therefore CLEARS the flags by design;
/// its `-m` merge path goes through `merge_tree_into_index`, which preserves
/// them through the update helper.
const CLEAR_ON_REBUILD: &[&str] = &["src/command/read_tree.rs"];

#[test]
fn staging_commands_route_replacements_through_the_flag_helper() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut problems = Vec::new();
    let mut helper_users = 0usize;

    for module in STAGING_MODULES {
        let source = fs::read_to_string(root.join(module))
            .unwrap_or_else(|error| panic!("read {module}: {error}"));
        let production = source.split("#[cfg(test)]").next().unwrap_or("");
        if production.contains("update_preserving_flags") {
            helper_users += 1;
        }
        for (index, line) in production.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if !(line.contains("index.update(") || line.contains("index.add(")) {
                continue;
            }
            if line.contains("update_preserving_flags") {
                continue;
            }
            let allowed = ALLOWED.iter().any(|(allowed_module, fragment)| {
                *allowed_module == *module && line.contains(fragment)
            });
            if !allowed {
                problems.push(format!("{module}:{}: {}", index + 1, trimmed));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "unclassified direct index replacement(s) bypass update_preserving_flags:\n{}",
        problems.join("\n")
    );
    assert!(
        helper_users >= 5,
        "expected the staging modules to use the helper (found {helper_users})"
    );

    for module in REBUILD_MODULES {
        let source = fs::read_to_string(root.join(module))
            .unwrap_or_else(|error| panic!("read {module}: {error}"));
        assert!(
            source.contains("preserve_skip_worktree_from"),
            "{module} rebuilds the index and must carry skip-worktree with preserve_skip_worktree_from"
        );
    }
    for module in CLEAR_ON_REBUILD {
        let source = fs::read_to_string(root.join(module))
            .unwrap_or_else(|error| panic!("read {module}: {error}"));
        assert!(
            source.contains("merge_tree_into_index"),
            "{module} must route its -m merge path through merge_tree_into_index"
        );
    }
}
