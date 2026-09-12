//! End-to-end coverage for `merge --quit`.

use std::fs;

use super::{
    assert_cli_success, create_committed_repo_via_cli, create_diverged_repo_for_conflict,
    head_commit, parse_cli_error_stderr, run_libra_command, stash_list_len,
};

#[test]
fn merge_quit_removes_state_without_changing_a_conflict() {
    let repo = create_diverged_repo_for_conflict();
    let root = repo.path();
    let head_before = head_commit(root);

    let conflicted = run_libra_command(&["merge", "feature"], root);
    assert_eq!(conflicted.status.code(), Some(128), "set up a conflict");
    let index_before = run_libra_command(&["ls-files", "-s"], root).stdout;
    let worktree_before = fs::read(root.join("shared.txt")).expect("read conflict markers");
    assert!(root.join(".libra/merge-state.json").exists());

    let quit = run_libra_command(&["merge", "--quit"], root);
    assert_cli_success(&quit, "quit merge");
    assert!(
        String::from_utf8_lossy(&quit.stdout).contains("Merge state cleared"),
        "quit should explain that it leaves the conflict checkout intact: {}",
        String::from_utf8_lossy(&quit.stdout)
    );
    assert_eq!(head_commit(root), head_before, "quit must not move HEAD");
    assert!(
        !root.join(".libra/merge-state.json").exists(),
        "quit must remove merge state"
    );
    assert_eq!(
        run_libra_command(&["ls-files", "-s"], root).stdout,
        index_before,
        "quit must retain every conflict index stage"
    );
    assert_eq!(
        fs::read(root.join("shared.txt")).expect("read conflict markers after quit"),
        worktree_before,
        "quit must retain the conflict worktree"
    );
}

#[test]
fn merge_quit_reports_when_no_merge_is_in_progress() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["merge", "--quit"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert_eq!(report.message, "no merge in progress");
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("libra merge <branch>")),
        "the error should tell the user how to start a merge: {:?}",
        report.hints
    );
}

#[test]
fn merge_quit_rejects_other_merge_options() {
    let repo = create_committed_repo_via_cli();
    for argv in [
        &["merge", "--quit", "feature"][..],
        &["merge", "--quit", "--continue"][..],
        &["merge", "--quit", "--no-edit"][..],
        &["merge", "--quit", "--autostash"][..],
        &["merge", "--quit", "--signoff"][..],
    ] {
        let output = run_libra_command(argv, repo.path());
        assert_eq!(
            output.status.code(),
            Some(129),
            "clap must reject {argv:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn merge_quit_promotes_a_held_autostash_without_changing_the_conflict() {
    let repo = create_diverged_repo_for_conflict();
    let root = repo.path();
    fs::write(root.join("unrelated.txt"), "precious\n").expect("write dirty file");
    assert_cli_success(
        &run_libra_command(&["add", "unrelated.txt"], root),
        "stage dirty file",
    );
    let conflicted = run_libra_command(&["merge", "feature", "--autostash"], root);
    assert_eq!(
        conflicted.status.code(),
        Some(128),
        "set up a held autostash"
    );
    let index_before = run_libra_command(&["ls-files", "-s"], root).stdout;
    let worktree_before = fs::read(root.join("shared.txt")).expect("read conflict markers");
    assert_eq!(stash_list_len(root), 0, "autostash starts held");
    assert!(root.join(".libra/merge-autostash.json").exists());

    assert_cli_success(&run_libra_command(&["merge", "--quit"], root), "quit merge");

    assert!(!root.join(".libra/merge-state.json").exists());
    assert!(
        !root.join(".libra/merge-autostash.json").exists(),
        "quit must not leave the held stash as an invisible orphan"
    );
    assert_eq!(stash_list_len(root), 1, "quit promotes the held autostash");
    assert!(
        !root.join("unrelated.txt").exists(),
        "quit must leave the conflict checkout intact rather than re-applying the stash"
    );
    assert_eq!(
        run_libra_command(&["ls-files", "-s"], root).stdout,
        index_before,
        "quit must retain every conflict index stage while promoting autostash"
    );
    assert_eq!(
        fs::read(root.join("shared.txt")).expect("read conflict markers after quit"),
        worktree_before,
        "quit must retain the conflict worktree while promoting autostash"
    );
}
