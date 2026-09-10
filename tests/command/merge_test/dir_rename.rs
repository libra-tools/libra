//! MG-07: directory renames relocate paths the other side added below the old name.

use super::{
    assert_cli_success, create_committed_repo_via_cli, head_commit, index_stage_lines,
    merge_expecting_conflict, parse_json_stdout, run_libra_command,
    run_libra_command_with_stdin_and_env,
};

fn directory_rename_repo_with_collision(collision: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_all = |message: &str| {
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage fixture");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
            "commit fixture",
        );
    };

    std::fs::create_dir_all(p.join("old")).expect("create source directory");
    std::fs::write(p.join("old/a"), "a\n").expect("write old/a");
    std::fs::write(p.join("old/b"), "b\n").expect("write old/b");
    commit_all("directory rename base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "feature branch",
    );

    std::fs::rename(p.join("old"), p.join("new")).expect("rename directory on main");
    if collision {
        std::fs::write(p.join("new/c"), "main addition\n").expect("add colliding target");
    }
    commit_all("rename old to new");

    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    std::fs::write(p.join("old/a"), "feature edit\n").expect("modify path below old name");
    std::fs::write(p.join("old/c"), "feature addition\n").expect("add path below old name");
    commit_all("modify and add below old directory");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    repo
}

fn directory_rename_repo() -> tempfile::TempDir {
    directory_rename_repo_with_collision(false)
}

fn split_directory_rename_repo_with_addition(add_below_split: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_all = |message: &str| {
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage fixture");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
            "commit fixture",
        );
    };
    std::fs::create_dir_all(p.join("old")).expect("create source directory");
    std::fs::write(p.join("old/a"), "a\n").expect("write old/a");
    std::fs::write(p.join("old/b"), "b\n").expect("write old/b");
    commit_all("split base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "feature branch",
    );
    std::fs::create_dir_all(p.join("new1")).expect("create first destination");
    std::fs::create_dir_all(p.join("new2")).expect("create second destination");
    std::fs::rename(p.join("old/a"), p.join("new1/a")).expect("first rename");
    std::fs::rename(p.join("old/b"), p.join("new2/b")).expect("second rename");
    commit_all("split directory rename");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    if add_below_split {
        std::fs::write(p.join("old/c"), "feature addition\n").expect("add below split source");
    } else {
        std::fs::write(p.join("feature.txt"), "unrelated\n").expect("add unrelated feature path");
    }
    commit_all("change feature side");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    repo
}

fn split_directory_rename_repo() -> tempfile::TempDir {
    split_directory_rename_repo_with_addition(true)
}

fn partial_directory_rename_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_all = |message: &str| {
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage fixture");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
            "commit fixture",
        );
    };
    std::fs::create_dir_all(p.join("old")).expect("create source directory");
    std::fs::write(p.join("old/a"), "a\n").expect("write old/a");
    std::fs::write(p.join("old/b"), "b\n").expect("write old/b");
    commit_all("partial base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "feature branch",
    );
    std::fs::create_dir_all(p.join("new")).expect("create destination");
    std::fs::rename(p.join("old/a"), p.join("new/a")).expect("move only one file");
    commit_all("partial directory move");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    std::fs::write(p.join("old/c"), "feature addition\n").expect("add below old directory");
    commit_all("add below old directory");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    repo
}

#[test]
fn merge_dir_rename_true_moves_added_path() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Given: main moved every tracked file from old/ to new/, while the
        // other side added a new path below old/.
        let repo = directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );

        // When: the branches are merged through either tree-walk engine.
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);

        // Then: the addition follows the inferred directory rename.
        assert_cli_success(&output, "merge directory rename");
        assert_eq!(
            std::fs::read_to_string(p.join("new/c")).expect("relocated addition"),
            "feature addition\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("new/a")).expect("per-file rename carries edit"),
            "feature edit\n"
        );
        assert!(!p.join("old/c").exists(), "the old path must not survive");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to new/c"),
            "the human output reports the relocation: {output:?}"
        );
    }
}

#[test]
fn merge_dir_rename_true_moves_an_addition_from_the_current_side_too() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "reverse merge",
        );
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        let output = run_libra_command_with_stdin_and_env(&["merge", "main"], p, "", env);
        assert_cli_success(&output, "reverse directory rename merge");
        assert_eq!(
            std::fs::read_to_string(p.join("new/c")).expect("relocated current-side addition"),
            "feature addition\n"
        );
        assert!(!p.join("old/c").exists());
        assert!(String::from_utf8_lossy(&output.stdout).contains("moving it to new/c"));
    }
}

#[test]
fn merge_dir_rename_does_not_infer_a_directory_that_still_exists() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = partial_directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&output, "partial directory move");
        assert_eq!(
            std::fs::read_to_string(p.join("old/c")).expect("unmoved addition"),
            "feature addition\n"
        );
        assert!(!p.join("new/c").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Path updated:"));
    }
}

#[test]
fn merge_dir_rename_false_leaves_added_path_at_the_old_name() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "false"], p),
            "disable directory renames",
        );
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&output, "merge without directory renames");
        assert_eq!(
            std::fs::read_to_string(p.join("old/c")).expect("addition stays put"),
            "feature addition\n"
        );
        assert!(!p.join("new/c").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Path updated:"));
    }
}

#[test]
fn merge_renames_false_also_disables_directory_rename_inference() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        assert_cli_success(
            &run_libra_command(&["config", "merge.renames", "false"], p),
            "disable all rename detection",
        );
        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert_eq!(
            std::fs::read_to_string(p.join("old/c")).expect("addition stays put"),
            "feature addition\n"
        );
        assert!(!p.join("new/c").exists());
        assert_eq!(
            index_stage_lines(p, "old/c").len(),
            1,
            "the unrelated addition remains a resolved stage-0 path"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Path updated:"));
    }
}

#[test]
fn merge_dir_rename_default_conflict_relocates_but_leaves_one_stage() {
    for explicit in [false, true] {
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            let repo = directory_rename_repo();
            let p = repo.path();
            if explicit {
                assert_cli_success(
                    &run_libra_command(&["config", "merge.directoryRenames", "conflict"], p),
                    "select conflict mode",
                );
            }
            let original_head = head_commit(p);
            let output = merge_expecting_conflict(p, &["merge", "feature"], env);
            assert_eq!(head_commit(p), original_head);
            assert!(String::from_utf8_lossy(&output.stdout).contains("CONFLICT (file location):"));
            assert_eq!(
                std::fs::read_to_string(p.join("new/c")).expect("suggested destination"),
                "feature addition\n"
            );
            assert!(!p.join("old/c").exists());
            let stages = index_stage_lines(p, "new/c");
            assert_eq!(
                stages.len(),
                1,
                "one side added the relocated path: {stages:?}"
            );
            assert!(stages[0].contains(" 3\t"), "theirs is stage 3: {stages:?}");
            assert_cli_success(
                &run_libra_command(&["add", "new/c"], p),
                "accept suggested location",
            );
            assert_cli_success(
                &run_libra_command(&["merge", "--continue", "--no-verify"], p),
                "finish directory-location conflict",
            );
            assert_eq!(
                std::fs::read_to_string(p.join("new/c")).expect("committed destination"),
                "feature addition\n"
            );
        }
    }
}

#[test]
fn merge_dir_rename_dry_run_json_reports_the_suggested_path_without_writes() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = directory_rename_repo();
        let p = repo.path();
        let original_head = head_commit(p);
        let output = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "feature", "--dry-run"],
            p,
            "",
            env,
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(head_commit(p), original_head);
        let json = parse_json_stdout(&output);
        assert_eq!(json["data"]["would_conflict"], true);
        assert_eq!(
            json["data"]["conflict_kinds"],
            serde_json::json!([{"path":"new/c", "kind":"directory-rename"}])
        );
        assert!(!p.join("new/c").exists());
        assert!(!p.join("old/c").exists());
        assert!(!p.join(".libra/merge-state.json").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("CONFLICT (file location):"));
    }
}

#[test]
fn merge_dir_rename_composes_with_an_add_add_at_the_destination() {
    for mode in ["true", "conflict"] {
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            let repo = directory_rename_repo_with_collision(true);
            let p = repo.path();
            assert_cli_success(
                &run_libra_command(&["config", "merge.directoryRenames", mode], p),
                "configure directory renames",
            );
            let output = merge_expecting_conflict(p, &["merge", "feature"], env);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let notice = if mode == "true" {
                "Path updated: old/c"
            } else {
                "CONFLICT (file location): old/c"
            };
            assert!(stdout.contains(notice), "{stdout}");
            let stages = index_stage_lines(p, "new/c");
            assert_eq!(stages.len(), 2, "rename/add stays an add/add: {stages:?}");
            assert!(stages.iter().any(|line| line.contains(" 2\t")));
            assert!(stages.iter().any(|line| line.contains(" 3\t")));
            let body = std::fs::read_to_string(p.join("new/c")).expect("conflict markers");
            assert!(body.contains("main addition") && body.contains("feature addition"));
            assert!(!p.join("old/c").exists());
        }
    }
}

#[test]
fn merge_dir_rename_split_is_a_deterministic_conflict_without_unmerged_stages() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = split_directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        let original_head = head_commit(p);
        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert_eq!(head_commit(p), original_head);
        assert!(String::from_utf8_lossy(&output.stdout).contains(
            "CONFLICT (directory rename split): Unclear where to rename old to; it was renamed to multiple other directories, with no destination getting a majority of the files."
        ));
        let stages = index_stage_lines(p, "old/c");
        assert_eq!(
            stages.len(),
            1,
            "Git leaves the addition at stage 0: {stages:?}"
        );
        assert!(stages[0].contains(" 0\t"), "{stages:?}");
        assert_eq!(
            std::fs::read_to_string(p.join("old/c")).expect("unmoved addition"),
            "feature addition\n"
        );
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "a split with stage-0 paths can be committed directly",
        );
        assert_ne!(head_commit(p), original_head);
    }
}

#[test]
fn merge_dir_rename_split_without_an_affected_addition_is_clean() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = split_directory_rename_repo_with_addition(false);
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&output, "an irrelevant directory split");
        assert!(p.join("feature.txt").is_file());
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("CONFLICT (directory rename split):")
        );
    }
}

#[test]
fn merge_dir_rename_split_can_be_aborted_without_leaving_the_stage_zero_addition() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = split_directory_rename_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["config", "merge.directoryRenames", "true"], p),
            "enable directory renames",
        );
        let original_head = head_commit(p);
        merge_expecting_conflict(p, &["merge", "feature"], env);
        assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort split");
        assert_eq!(head_commit(p), original_head);
        assert!(!p.join("old/c").exists());
        assert!(p.join("new1/a").is_file());
        assert!(p.join("new2/b").is_file());
        assert!(!p.join(".libra/merge-state.json").exists());
    }
}

#[test]
fn merge_dir_rename_rejects_unknown_configuration_before_writes() {
    let repo = directory_rename_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.directoryRenames", "conservative"], p),
        "set unsupported value",
    );
    let original_head = head_commit(p);
    let output = run_libra_command(&["merge", "feature"], p);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(head_commit(p), original_head);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("merge.directoryRenames"), "{stderr}");
    assert!(stderr.contains("true, false or conflict"), "{stderr}");
    assert!(!p.join("new/c").exists());
    assert!(!p.join(".libra/merge-state.json").exists());
}
