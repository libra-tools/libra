//! End-to-end coverage for the conflicted-merge `AUTO_MERGE` tree projection.

use std::fs;

use tempfile::tempdir;

use super::{assert_cli_success, commit_file, configure_identity_via_cli, run_libra_command};

fn conflicted_merge_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("create AUTO_MERGE repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize AUTO_MERGE repository",
    );
    configure_identity_via_cli(path);
    commit_file(path, "shared.txt", "base\n", "base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature",
    );
    commit_file(path, "shared.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main",
    );
    commit_file(path, "shared.txt", "main\n", "main change");
    repo
}

#[test]
fn merge_auto_merge_ref_is_a_conflict_lifetime_tree_and_not_a_global_resolver_alias() {
    let repo = conflicted_merge_repo();
    let path = repo.path();
    let merge = run_libra_command(&["merge", "feature"], path);
    assert_eq!(merge.status.code(), Some(128), "merge must conflict");

    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(path.join(".libra/merge-state.json")).expect("read merge state"),
    )
    .expect("parse merge state");
    let tree = state["auto_merge"]
        .as_str()
        .expect("conflicted state records AUTO_MERGE tree")
        .to_string();

    let rev_parse = run_libra_command(&["rev-parse", "AUTO_MERGE"], path);
    assert_cli_success(&rev_parse, "resolve AUTO_MERGE in rev-parse");
    assert_eq!(
        String::from_utf8(rev_parse.stdout)
            .expect("rev-parse output is utf-8")
            .trim(),
        tree
    );
    let typed_tree = run_libra_command(&["rev-parse", "AUTO_MERGE^{tree}"], path);
    assert_cli_success(&typed_tree, "peel AUTO_MERGE as a tree");
    assert_eq!(
        String::from_utf8(typed_tree.stdout)
            .expect("typed rev-parse output is utf-8")
            .trim(),
        tree
    );
    let kind = run_libra_command(&["cat-file", "-t", "AUTO_MERGE"], path);
    assert_cli_success(&kind, "inspect AUTO_MERGE type");
    assert_eq!(
        String::from_utf8(kind.stdout)
            .expect("cat-file type output is utf-8")
            .trim(),
        "tree"
    );
    let marker = run_libra_command(&["cat-file", "-p", "AUTO_MERGE:shared.txt"], path);
    assert_cli_success(&marker, "read a conflict marker through AUTO_MERGE");
    let marker = String::from_utf8(marker.stdout).expect("AUTO_MERGE blob is utf-8");
    assert!(marker.contains("<<<<<<< HEAD"));
    assert!(marker.contains("main\n"));
    assert!(marker.contains("feature\n"));
    assert!(marker.contains(">>>>>>>"));
    assert_cli_success(
        &run_libra_command(&["diff", "AUTO_MERGE"], path),
        "diff accepts the AUTO_MERGE tree-ish",
    );

    let update = run_libra_command(&["update-ref", "refs/heads/no-auto", "AUTO_MERGE"], path);
    assert!(
        !update.status.success(),
        "AUTO_MERGE must not leak into update-ref's global resolver"
    );

    assert_cli_success(
        &run_libra_command(&["merge", "--abort"], path),
        "abort merge",
    );
    assert!(
        !run_libra_command(&["rev-parse", "AUTO_MERGE"], path)
            .status
            .success(),
        "AUTO_MERGE must disappear with the merge state"
    );
}

#[test]
fn merge_auto_merge_ref_is_removed_by_continue_quit_and_squash_conflicts() {
    let continued = conflicted_merge_repo();
    let continued_path = continued.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], continued_path)
            .status
            .code(),
        Some(128),
        "merge must conflict before continue"
    );
    fs::write(continued_path.join("shared.txt"), "resolved\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], continued_path),
        "stage resolution",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], continued_path),
        "continue merge",
    );
    assert!(
        !run_libra_command(&["rev-parse", "AUTO_MERGE"], continued_path)
            .status
            .success(),
        "AUTO_MERGE must disappear after continue"
    );

    let quit = conflicted_merge_repo();
    let quit_path = quit.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], quit_path)
            .status
            .code(),
        Some(128),
        "merge must conflict before quit"
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--quit"], quit_path),
        "quit merge",
    );
    assert!(
        !run_libra_command(&["rev-parse", "AUTO_MERGE"], quit_path)
            .status
            .success(),
        "AUTO_MERGE must disappear after quit"
    );

    let squash = conflicted_merge_repo();
    let squash_path = squash.path();
    assert_eq!(
        run_libra_command(&["merge", "--squash", "feature"], squash_path)
            .status
            .code(),
        Some(128),
        "squash merge must conflict"
    );
    assert!(
        !squash_path.join(".libra/merge-state.json").exists(),
        "a conflicted squash must not create merge state"
    );
    assert!(
        !run_libra_command(&["rev-parse", "AUTO_MERGE"], squash_path)
            .status
            .success(),
        "AUTO_MERGE must not exist for a squash conflict"
    );
}
