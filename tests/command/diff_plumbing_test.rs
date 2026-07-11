//! Integration tests for the diff plumbing trio: `diff-tree` / `diff-index` /
//! `diff-files`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, process::Output};

use tempfile::TempDir;

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

fn out_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn rev_parse(repo: &TempDir, rev: &str) -> String {
    String::from_utf8_lossy(&run_libra_command(&["rev-parse", rev], repo.path()).stdout)
        .trim()
        .to_string()
}

/// A repo with two commits that differ in `tracked.txt`; returns `(repo, c1, c2)`.
fn repo_with_two_commits() -> (TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let c1 = rev_parse(&repo, "HEAD");
    fs::write(repo.path().join("tracked.txt"), "second-content\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage second change",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "second commit",
    );
    let c2 = rev_parse(&repo, "HEAD");
    (repo, c1, c2)
}

fn configure_invalid_diff_renames(repo: &TempDir) {
    assert_cli_success(
        &run_libra_command(&["config", "diff.renames", "sideways"], repo.path()),
        "set invalid porcelain-only diff.renames",
    );
}

#[test]
fn diff_plumbing_tree_diffs_two_trees() {
    let (repo, c1, c2) = repo_with_two_commits();
    let out = run_libra_command(&["diff-tree", &c1, &c2], repo.path());
    // Plumbing diff exits 1 when there are differences.
    assert_eq!(
        out.status.code(),
        Some(1),
        "diff-tree should exit 1 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-tree should show the changed file: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_tree_ignores_porcelain_rename_config() {
    let (repo, c1, c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    let out = run_libra_command(&["diff-tree", &c1, &c2], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_files_shows_unstaged_changes() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // Modify the working tree without staging.
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-files"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "diff-files should exit 1 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-files should show the unstaged change: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_files_ignores_porcelain_rename_config() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-files"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_index_diffs_tree_against_worktree() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-index", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "diff-index should exit 1 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-index should show the change vs the working tree: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_index_ignores_porcelain_rename_config() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-index", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_index_cached_is_unsupported() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(&["diff-index", "--cached", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "diff-index --cached is not yet supported"
    );
}

#[test]
fn diff_plumbing_tree_respects_pathspec() {
    let (repo, c1, c2) = repo_with_two_commits();
    // Limit to an unrelated path: no diff for it.
    let out = run_libra_command(&["diff-tree", &c1, &c2, "--", "nonexistent"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !out_str(&out).contains("tracked.txt"),
        "a path limiter should exclude tracked.txt: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_outside_repository_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_libra_command(&["diff-files"], dir.path());
    assert_eq!(out.status.code(), Some(128));
}
