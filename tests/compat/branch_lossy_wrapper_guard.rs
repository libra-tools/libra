#[path = "../helpers/workspace_sources.rs"]
mod workspace_sources;

use std::{fs, path::PathBuf};

/// Patterns that the guard scans for in `src/` to ensure production code
/// never re-introduces calls to lossy branch-store wrappers. As of v0.17.222
/// the five lossy wrappers + their `_with_conn` variants have been deleted
/// from `src/internal/branch.rs`, so any production call to these names is
/// already a compile error — the guard remains as defense-in-depth that
/// surfaces a clearer message if a contributor reintroduces the wrappers
/// and then adds a caller.
const LOSSY_BRANCH_CALLS: [&str; 10] = [
    "Branch::find_branch(",
    "Branch::list_branches(",
    "Branch::delete_branch(",
    "Branch::exists(",
    "Branch::search_branch(",
    "InternalBranch::find_branch(",
    "InternalBranch::list_branches(",
    "InternalBranch::delete_branch(",
    "InternalBranch::exists(",
    "InternalBranch::search_branch(",
];

#[test]
fn production_code_uses_fallible_branch_store_apis() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let internal_branch = repo_root.join("src").join("internal").join("branch.rs");
    let files = workspace_sources::collect_workspace_rust_files(&repo_root);

    let mut offenders = Vec::new();
    for file in files {
        if file == internal_branch {
            continue;
        }
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read '{}': {error}", file.display()));
        for (line_index, line) in text.lines().enumerate() {
            for pattern in LOSSY_BRANCH_CALLS {
                if line.contains(pattern) {
                    let rel = file
                        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&file);
                    offenders.push(format!(
                        "{}:{} uses lossy branch wrapper '{}'",
                        rel.display(),
                        line_index + 1,
                        pattern.trim_end_matches('('),
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production code must call branch *_result APIs instead of lossy compatibility wrappers:\n{}",
        offenders.join("\n"),
    );
}
