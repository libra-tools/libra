//! MG-06: rename conflicts compose with rerere and the gitlink safety boundary.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use super::{
    GITLINK_BASE, GITLINK_MOVED, assert_cli_success, create_gitlink_repo, gitlink_tree_line,
    head_commit, index_stage_lines, merge_expecting_conflict, parse_cli_error_stderr,
    rename_1to2_repo, run_libra_command, run_libra_command_with_stdin_and_env, stage_blob,
    stdout_trimmed,
};

#[test]
fn merge_rename_conflict_rerere_replays_after_abort_with_configured_staging() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for (auto_update, rerere_flags, expected_auto_stage) in [
            (false, &[][..], false),
            (true, &[][..], true),
            (true, &["--no-rerere-autoupdate"][..], false),
            (false, &["--rerere-autoupdate"][..], true),
        ] {
            // Given: the same labelled 1to2 content conflict occurs twice, so
            // rerere's normalized hunk key identifies the recorded resolution.
            let repo = rename_1to2_repo(2, 2);
            let p = repo.path();
            let original_head = head_commit(p);
            let original_a = std::fs::read(p.join("a")).expect("ours before merge");
            for (key, value) in [
                ("rerere.enabled", "true"),
                (
                    "rerere.autoUpdate",
                    if auto_update { "true" } else { "false" },
                ),
            ] {
                assert_cli_success(
                    &run_libra_command(&["config", key, value], p),
                    "rerere config",
                );
            }
            let mut merge_args = vec!["merge", "feature"];
            merge_args.extend_from_slice(rerere_flags);
            merge_expecting_conflict(p, &merge_args, env);
            let conflict = std::fs::read_to_string(p.join("a")).expect("rename markers");
            assert!(conflict.contains("<<<<<<<< HEAD:a"));
            assert!(conflict.contains(">>>>>>>> feature:b"));
            assert_eq!(
                std::fs::read_to_string(p.join("b")).expect("other destination"),
                conflict
            );
            let tracked = run_libra_command(&["rerere", "status"], p);
            assert_cli_success(&tracked, "rerere tracks both rename destinations");
            let mut paths: Vec<_> = String::from_utf8_lossy(&tracked.stdout)
                .lines()
                .map(str::to_owned)
                .collect();
            paths.sort();
            assert_eq!(paths, ["a", "b"]);
            for path in ["a", "b"] {
                std::fs::write(p.join(path), "resolved rename content\n")
                    .expect("resolve both paths");
            }
            assert_cli_success(
                &run_libra_command(&["rerere"], p),
                "record resolution before abort",
            );
            assert_cli_success(
                &run_libra_command(&["merge", "--abort"], p),
                "abort first merge",
            );
            assert_eq!(head_commit(p), original_head);
            assert_eq!(
                std::fs::read(p.join("a")).expect("restored ours"),
                original_a
            );
            assert!(!p.join("b").exists());

            // When: the real merge recreates exactly the recorded conflict.
            let replay = merge_expecting_conflict(p, &merge_args, env);

            // Then: both resolutions return; only configured autoUpdate stages
            // them, while the merge remains explicitly resumable.
            let stdout = String::from_utf8_lossy(&replay.stdout);
            for (path, unmerged_stage) in [("a", 2), ("b", 3)] {
                assert!(stdout.contains(&format!(
                    "Resolved '{path}' using a previously recorded resolution."
                )));
                assert_eq!(
                    std::fs::read_to_string(p.join(path)).expect("replayed bytes"),
                    "resolved rename content\n"
                );
                let stages = index_stage_lines(p, path);
                assert_eq!(stages.len(), 1, "{path}: {stages:?}");
                let expected = if expected_auto_stage {
                    0
                } else {
                    unmerged_stage
                };
                assert!(
                    stages[0].contains(&format!(" {expected}\t")),
                    "{path}: {stages:?}"
                );
            }
            assert_eq!(head_commit(p), original_head);
            if !expected_auto_stage {
                assert_cli_success(
                    &run_libra_command(&["add", "a", "b"], p),
                    "stage reused resolution",
                );
            }
            assert_cli_success(
                &run_libra_command(&["merge", "--continue", "--no-verify"], p),
                "finish reused rename resolution",
            );
        }
    }
}

/// Extend the existing gitlink fixture with two pure renames of tracked.txt.
/// Plumbing builds the feature tree without checking out a changed submodule.
fn gitlink_rename_repo(feature_gitlink: &str) -> tempfile::TempDir {
    let repo = create_gitlink_repo(feature_gitlink);
    let p = repo.path();
    std::fs::rename(p.join("tracked.txt"), p.join("a")).expect("ours renames");
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "tracked.txt"], p),
        "remove ours source",
    );
    assert_cli_success(
        &run_libra_command(&["add", "a"], p),
        "stage ours destination",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
        "commit ours rename",
    );
    let content = stage_blob(&index_stage_lines(p, "a")[0]);
    let feature = run_libra_command(&["rev-parse", "feature"], p);
    assert_cli_success(&feature, "feature tip");
    let feature = stdout_trimmed(&feature);
    assert_cli_success(
        &run_libra_command(&["read-tree", "feature"], p),
        "feature index",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--remove",
                "tracked.txt",
                "--cacheinfo",
                &format!("100644,{content},b"),
            ],
            p,
        ),
        "stage theirs rename",
    );
    let tree = run_libra_command(&["write-tree"], p);
    assert_cli_success(&tree, "feature tree");
    let tree = stdout_trimmed(&tree);
    let commit = run_libra_command(
        &["commit-tree", &tree, "-p", &feature, "-m", "theirs renames"],
        p,
    );
    assert_cli_success(&commit, "feature rename commit");
    assert_cli_success(
        &run_libra_command(
            &["update-ref", "refs/heads/feature", &stdout_trimmed(&commit)],
            p,
        ),
        "advance feature",
    );
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], p),
        "restore main index",
    );
    repo
}

#[test]
fn merge_rename_conflict_keeps_an_agreed_gitlink_through_staging_and_continue() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Given: all three inputs agree on vendor while both sides rename a file.
        let repo = gitlink_rename_repo(GITLINK_BASE);
        let p = repo.path();
        let vendor_before = index_stage_lines(p, "vendor");
        assert_eq!(vendor_before, [format!("160000 {GITLINK_BASE} 0\tvendor")]);

        // When: the path-level conflict is presented, staged and continued.
        let conflict = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(String::from_utf8_lossy(&conflict.stdout).contains("CONFLICT (rename/rename)"));
        assert_eq!(index_stage_lines(p, "vendor"), vendor_before);
        assert_cli_success(
            &run_libra_command(&["add", "a", "b"], p),
            "stage renamed destinations",
        );
        assert_eq!(index_stage_lines(p, "vendor"), vendor_before);
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "finish rename with gitlink",
        );

        // Then: the stage-0 pointer and committed tree both preserve its OID.
        assert_eq!(index_stage_lines(p, "vendor"), vendor_before);
        assert_eq!(
            gitlink_tree_line(p, "HEAD"),
            Some(format!("160000 commit {GITLINK_BASE}\tvendor"))
        );
        assert_eq!(
            std::fs::read(p.join("a")).expect("ours destination"),
            b"tracked\n"
        );
        assert_eq!(
            std::fs::read(p.join("b")).expect("theirs destination"),
            b"tracked\n"
        );
        assert!(!p.join(".libra/merge-state.json").exists());
    }
}

/// The fixture has only root-level worktree files. Include directory names so
/// even creating a new submodule placeholder is observable, excluding metadata.
fn fixture_worktree(repo: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    std::fs::read_dir(repo)
        .expect("worktree entries")
        .map(|entry| entry.expect("worktree entry"))
        .filter(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".libra" | ".libra-test-home")
            )
        })
        .map(|entry| {
            let bytes = if entry.file_type().expect("entry type").is_dir() {
                None
            } else {
                Some(std::fs::read(entry.path()).expect("worktree bytes"))
            };
            (PathBuf::from(entry.file_name()), bytes)
        })
        .collect()
}

#[test]
fn merge_rename_conflict_refuses_a_changed_gitlink_without_any_worktree_or_index_write() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Given: a real 1to2 rename conflict alongside a changed gitlink.
        let repo = gitlink_rename_repo(GITLINK_MOVED);
        let p = repo.path();
        let head_before = head_commit(p);
        let index_before = std::fs::read(p.join(".libra/index")).expect("index before merge");
        let files_before = fixture_worktree(p);

        // When: gitlink arbitration is refused before rename conflict writes.
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);

        // Then: HEAD, raw index, every fixture worktree entry and merge state
        // prove the fail-closed boundary, not merely an error code.
        assert_eq!(output.status.code(), Some(128));
        let (stderr, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
        assert!(stderr.contains("vendor"));
        assert_eq!(head_commit(p), head_before);
        assert_eq!(
            std::fs::read(p.join(".libra/index")).expect("index after refusal"),
            index_before
        );
        assert_eq!(fixture_worktree(p), files_before);
        assert!(!p.join(".libra/merge-state.json").exists());
    }
}
