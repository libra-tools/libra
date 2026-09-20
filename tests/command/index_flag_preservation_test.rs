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

/// SW-04 (M-KEEP-B B1/B2): reset --hard / mixed reset and checkout/switch
/// preserve the skip-worktree bit on untouched paths.
#[test]
fn test_history_commands_preserve_skip_worktree_matrix() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("other"), "other\n").expect("write other");
    fs::write(root.join("s"), "s\n").expect("write s");
    assert_cli_success(&run_libra_command(&["add", "other", "s"], root), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    mark_skip_worktree(root, "s");

    // B1a: reset --hard.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD"], root),
        "B1 reset --hard",
    );
    assert_bit(root, "B1 reset --hard");

    // B1b: mixed reset after staging a change.
    fs::write(root.join("other"), "b1\n").expect("write b1");
    assert_cli_success(&run_libra_command(&["add", "other"], root), "stage b1");
    assert_cli_success(
        &run_libra_command(&["reset", "HEAD"], root),
        "B1 reset mixed",
    );
    assert_bit(root, "B1 reset mixed");

    // B2a: checkout -- other restores one path.
    fs::write(root.join("other"), "b2\n").expect("write b2");
    assert_cli_success(
        &run_libra_command(&["checkout", "--", "other"], root),
        "B2 checkout --",
    );
    assert_bit(root, "B2 checkout --");

    // B2b: switch round trip through a branch with a different tree.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "side"], root),
        "create side",
    );
    fs::write(root.join("side.txt"), "side\n").expect("write side");
    assert_cli_success(&run_libra_command(&["add", "side.txt"], root), "stage side");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "side", "--no-verify"], root),
        "commit side",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], root), "switch main");
    assert_bit(root, "B2 switch main");
    assert_cli_success(&run_libra_command(&["switch", "side"], root), "switch side");
    assert_bit(root, "B2 switch side");

    // B3a: fast-forward merge back on main.
    assert_cli_success(
        &run_libra_command(&["switch", "main"], root),
        "back to main",
    );
    assert_cli_success(&run_libra_command(&["merge", "side"], root), "B3 ff merge");
    assert_bit(root, "B3 ff merge");

    // B7: read-tree without -m clears the bit (Git rebuild semantics).
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "B7 read-tree",
    );
    assert!(
        !skip_worktree_set(root, "s"),
        "B7 read-tree without -m must clear the skip-worktree bit"
    );

    // B6: read-tree -m carries it back.
    mark_skip_worktree(root, "s");
    assert_cli_success(
        &run_libra_command(&["read-tree", "-m", "HEAD"], root),
        "B6 read-tree -m",
    );
    assert_bit(root, "B6 read-tree -m");
}

/// SW-04 (M-KEEP-B B4/B5): cherry-pick, revert, rebase and am that do not
/// touch the skip-worktree path preserve its bit.
#[test]
fn test_history_commands_b4_b5_preserve_skip_worktree() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("other"), "other\n").expect("write other");
    fs::write(root.join("s"), "s\n").expect("write s");
    assert_cli_success(&run_libra_command(&["add", "other", "s"], root), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    mark_skip_worktree(root, "s");

    // B4a: cherry-pick an unrelated addition.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "cp-src"], root),
        "create cp-src",
    );
    fs::write(root.join("cp.txt"), "cp\n").expect("write cp");
    assert_cli_success(&run_libra_command(&["add", "cp.txt"], root), "stage cp");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "cp", "--no-verify"], root),
        "commit cp",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], root), "switch main");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "cp-src"], root),
        "B4 cherry-pick",
    );
    assert_bit(root, "B4 cherry-pick");

    // B4b: revert an unrelated modification.
    fs::write(root.join("other"), "revert-me\n").expect("write other");
    assert_cli_success(&run_libra_command(&["add", "other"], root), "stage revert");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "to-revert", "--no-verify"], root),
        "commit to-revert",
    );
    assert_cli_success(
        &run_libra_command(&["revert", "--no-edit", "HEAD"], root),
        "B4 revert",
    );
    assert_bit(root, "B4 revert");

    // B4c: rebase a topic branch onto an advanced main.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "rb"], root),
        "create rb",
    );
    fs::write(root.join("rb.txt"), "rb\n").expect("write rb");
    assert_cli_success(&run_libra_command(&["add", "rb.txt"], root), "stage rb");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "rb", "--no-verify"], root),
        "commit rb",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], root), "switch main");
    fs::write(root.join("main2.txt"), "main2\n").expect("write main2");
    assert_cli_success(
        &run_libra_command(&["add", "main2.txt"], root),
        "stage main2",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main2", "--no-verify"], root),
        "commit main2",
    );
    assert_cli_success(&run_libra_command(&["switch", "rb"], root), "switch rb");
    assert_cli_success(&run_libra_command(&["rebase", "main"], root), "B4 rebase");
    assert_bit(root, "B4 rebase");

    // B5: apply a format-patch mail message.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "am-topic"], root),
        "create am-topic",
    );
    fs::write(root.join("am.txt"), "am\n").expect("write am");
    assert_cli_success(&run_libra_command(&["add", "am.txt"], root), "stage am");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "am topic", "--no-verify"], root),
        "commit am",
    );
    let patch = run_libra_command(&["format-patch", "-1", "HEAD"], root);
    assert!(
        patch.status.success(),
        "format-patch: {}",
        String::from_utf8_lossy(&patch.stderr)
    );
    // `format-patch` reports the written file (stdout or stderr depending on
    // the stream split); locate the `.patch` file in the repository root.
    let patch_file = std::fs::read_dir(root)
        .expect("read repo dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .find(|name| name.ends_with(".patch"))
        .expect("format-patch must write a .patch file");
    assert_cli_success(
        &run_libra_command(&["switch", "main"], root),
        "switch main before am",
    );
    assert_cli_success(&run_libra_command(&["am", &patch_file], root), "B5 am");
    assert_bit(root, "B5 am");
}

/// SW-04 (M-KEEP-B B7): `read-tree` without `-m` rebuilds the index and clears
/// the extended flags, matching Git's rebuild semantics.
#[test]
fn test_read_tree_without_merge_clears_flags() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("s"), "s\n").expect("write s");
    assert_cli_success(&run_libra_command(&["add", "s"], root), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    mark_skip_worktree(root, "s");
    assert!(skip_worktree_set(root, "s"), "precondition");

    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "read-tree",
    );
    assert!(
        !skip_worktree_set(root, "s"),
        "read-tree without -m must clear the extended flags"
    );
}
