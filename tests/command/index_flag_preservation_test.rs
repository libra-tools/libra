//! Index extended-flag preservation across the staging commands (plan
//! issues/490 SW-03, matrix M-KEEP-A A1–A7).
//!
//! No CLI can set skip-worktree until SW-07, so the tests set the bit through
//! the git-internal index API and then assert every staging command keeps it.

use std::{fs, path::Path};

use git_internal::{
    hash::HashKind,
    internal::index::{Index, IndexEntry},
};
use tempfile::tempdir;

use super::*;

fn index_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".libra/index")
}

/// Set `skip_worktree` on a tracked path through the index API (the CLI entry
/// point arrives with SW-07).
fn mark_skip_worktree(repo: &Path, path: &str) {
    let path_buf = index_path(repo);
    let mut index =
        Index::load_with_hash_kind(HashKind::Sha1, &path_buf).expect("load index for marking");
    let (hash, mode, size) = {
        let entry = index
            .get(path, 0)
            .unwrap_or_else(|| panic!("{path} must be tracked"));
        (entry.hash, entry.mode, entry.size)
    };
    let mut entry = IndexEntry::new_from_blob(path.to_string(), hash, size);
    entry.mode = mode;
    entry.flags.skip_worktree = true;
    index.update(entry);
    index
        .save_with_hash_kind(HashKind::Sha1, &path_buf)
        .expect("save index");
}

fn skip_worktree_set(repo: &Path, path: &str) -> bool {
    Index::load_with_hash_kind(HashKind::Sha1, index_path(repo))
        .expect("load index")
        .get(path, 0)
        .is_some_and(|entry| entry.flags.skip_worktree)
}

fn assert_bit(repo: &Path, leg: &str) {
    assert!(
        skip_worktree_set(repo, "s"),
        "{leg}: the skip-worktree bit on `s` must survive"
    );
}

#[test]
fn test_staging_commands_preserve_skip_worktree_matrix() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("other"), "other\n").expect("write other");
    fs::write(root.join("s"), "s\n").expect("write s");
    assert_cli_success(
        &run_libra_command(&["add", "other", "s"], root),
        "stage files",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    mark_skip_worktree(root, "s");

    // A1: update-index re-stages a modified file.
    fs::write(root.join("other"), "a1\n").expect("write a1");
    assert_cli_success(
        &run_libra_command(&["update-index", "other"], root),
        "A1 update-index",
    );
    assert_bit(root, "A1");

    // A2: add (plain, --renormalize, --chmod).
    fs::write(root.join("other"), "a2\n").expect("write a2");
    assert_cli_success(&run_libra_command(&["add", "other"], root), "A2 add");
    assert_bit(root, "A2 add");
    fs::write(root.join("other"), "a2b\n").expect("write a2b");
    assert_cli_success(
        &run_libra_command(&["add", "--renormalize", "other"], root),
        "A2 add --renormalize",
    );
    assert_bit(root, "A2 add --renormalize");
    fs::write(root.join("other"), "a2c\n").expect("write a2c");
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=+x", "other"], root),
        "A2 add --chmod",
    );
    assert_bit(root, "A2 add --chmod");

    // A3: commit -a stages the modified file.
    fs::write(root.join("other"), "a3\n").expect("write a3");
    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "a3", "--no-verify"], root),
        "A3 commit -a",
    );
    assert_bit(root, "A3 commit -a");

    // A4: stash push + pop.
    fs::write(root.join("other"), "a4\n").expect("write a4");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], root),
        "A4 stash push",
    );
    assert_bit(root, "A4 stash push");
    assert_cli_success(&run_libra_command(&["stash", "pop"], root), "A4 stash pop");
    assert_bit(root, "A4 stash pop");

    // A5: restore --staged.
    fs::write(root.join("other"), "a5\n").expect("write a5");
    assert_cli_success(&run_libra_command(&["add", "other"], root), "A5 stage");
    assert_cli_success(
        &run_libra_command(&["restore", "--staged", "other"], root),
        "A5 restore --staged",
    );
    assert_bit(root, "A5 restore --staged");

    // A6: mv carries the flags to the new path.
    assert_cli_success(
        &run_libra_command(&["mv", "other", "other2"], root),
        "A6 mv",
    );
    assert_bit(root, "A6 mv");
    let list = run_libra_command(&["ls-files"], root);
    let listed = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(
        listed.contains("other2") && !listed.contains("other\n"),
        "mv result: {listed}"
    );

    // A7: rm --cached removes only the named path.
    assert_cli_success(
        &run_libra_command(&["rm", "--cached", "other2"], root),
        "A7 rm --cached",
    );
    assert_bit(root, "A7 rm --cached");
    let list = run_libra_command(&["ls-files"], root);
    let listed = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(
        !listed.contains("other2"),
        "rm --cached removed other2: {listed}"
    );
    assert!(listed.contains("s"), "s stays tracked: {listed}");
}
