//! Global production unwrap audit guard.
//!
//! As of v0.17.267, every `.rs` file under `src/` is free of bare
//! `.unwrap()` calls in production code (i.e., anywhere outside an inline
//! `#[cfg(test)]` tests module or test-named files like `*_test.rs`,
//! `*_tests.rs`, `test.rs`, `tests.rs`). This guard scans the entire
//! source tree on every test run and fails the build if a contributor
//! reintroduces a bare `.unwrap()` anywhere in production code.
//!
//! Complements the per-file guards in `tests/compat/*_production_unwrap_guard.rs`,
//! which provide finer-grained failure messages identifying the specific
//! recovery convention for each file. This global guard is the
//! "everything else" net — it catches new unwraps in files that don't yet
//! have a dedicated guard.
//!
//! Scope and limits:
//!   - Skips files whose stem ends in `_test`, `_tests`, or whose basename
//!     is `test.rs` / `tests.rs` (those files are dedicated test modules).
//!   - Skips lines starting with `//` (comments and rustdoc).
//!   - Skips everything from the first `#[cfg(test)]` line onward.
//!   - Whitespace-tolerant: `.unwrap()` with any leading indent still trips.
//!
//! Doc-example unwraps inside `///` rustdoc fences are skipped by the
//! line-comment filter. Doc-example unwraps embedded in `//!` blocks are
//! also skipped for the same reason. If a new file legitimately needs
//! `.unwrap()` in doc-example code, place it after a `///` or `//!` prefix
//! on each line.

#[path = "../helpers/workspace_sources.rs"]
mod workspace_sources;

use std::{
    fs,
    path::{Path, PathBuf},
};

#[test]
fn entire_src_tree_has_no_bare_production_unwrap() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = workspace_sources::collect_workspace_rust_files(&repo_root);

    let mut offenders = Vec::new();
    for path in files {
        if is_test_file(&path) {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read '{}': {err}", path.display()));
        let mut in_tests_module = false;
        for (idx, raw_line) in text.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = raw_line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                in_tests_module = true;
            }
            if in_tests_module {
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains(".unwrap()") {
                let rel = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(&path);
                offenders.push(format!(
                    "{}:{} reintroduces bare `.unwrap()` in production code: \
                     use `.expect(\"INVARIANT: ...\")` or migrate the caller to a fallible \
                     sibling (see CLAUDE.md error handling rules)",
                    rel.display(),
                    line_number,
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production code under src/ must use INVARIANT-documented `.expect(\"...\")` \
         instead of bare `.unwrap()`:\n{}",
        offenders.join("\n"),
    );
}

fn is_test_file(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(stem, "test" | "tests") || stem.ends_with("_test") || stem.ends_with("_tests")
}
