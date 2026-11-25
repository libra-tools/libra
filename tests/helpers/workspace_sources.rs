//! Shared workspace-aware Rust source file collector for architecture and
//! compatibility guards.

use std::{
    fs,
    path::{Path, PathBuf},
};

const MODEL_ANCHOR: &str = "crates/libra-model/src/reference.rs";

/// Returns `[repo_root/src]` followed by every deterministically sorted
/// `repo_root/crates/*/src` directory.
#[allow(dead_code)]
pub fn workspace_source_roots(repo_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![repo_root.join("src")];
    let crates_dir = repo_root.join("crates");
    if crates_dir.is_dir() {
        let mut crate_srcs = Vec::new();
        for entry in fs::read_dir(&crates_dir)
            .unwrap_or_else(|err| panic!("failed to read '{}': {err}", crates_dir.display()))
        {
            let entry = entry.unwrap_or_else(|err| {
                panic!("failed to read entry in '{}': {err}", crates_dir.display())
            });
            let candidate = entry.path().join("src");
            if candidate.is_dir() {
                crate_srcs.push(candidate);
            }
        }
        crate_srcs.sort();
        roots.extend(crate_srcs);
    }
    roots
}

/// Collects every `.rs` file under `src/` and `crates/*/src/`, sorted
/// deterministically, and asserts on every call that the normalized
/// repo-relative path set includes `crates/libra-model/src/reference.rs`.
#[allow(dead_code)]
pub fn collect_workspace_rust_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = workspace_source_roots(repo_root);
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read '{}': {err}", dir.display()))
        {
            let path = entry
                .unwrap_or_else(|err| panic!("failed to read entry in '{}': {err}", dir.display()))
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    let has_model_anchor = files.iter().any(|path| {
        path.strip_prefix(repo_root)
            .ok()
            .is_some_and(|rel| rel.to_string_lossy().replace('\\', "/") == MODEL_ANCHOR)
    });
    assert!(
        has_model_anchor,
        "workspace source walk from '{}' must include '{}'",
        repo_root.display(),
        MODEL_ANCHOR,
    );
    files
}
