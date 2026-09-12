use super::*;

#[test]
fn merge_rename_conflict_squash_refuses_another_merge_before_resolution() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Given an unmerged squash index with no ordinary merge state.
        let repo = rename_add_repo();
        let p = repo.path();
        merge_expecting_conflict(p, &["merge", "--squash", "feature"], env);
        let head = run_libra_command(&["rev-parse", "HEAD"], p).stdout;
        let index = std::fs::read(p.join(".libra/index")).expect("index");
        let content = std::fs::read(p.join("new")).expect("conflict");
        // When even an up-to-date merge is requested, Git refuses to proceed.
        for args in [
            &["merge", "HEAD"][..],
            &["merge", "--dry-run", "feature"][..],
        ] {
            let out = run_libra_command(args, p);
            assert!(
                !out.status.success(),
                "unresolved index must reject {args:?}"
            );
            let error = String::from_utf8_lossy(&out.stderr);
            assert!(error.contains("libra commit"), "{error}");
            assert!(!error.contains("merge --continue"), "{error}");
            // Then every unresolved byte remains available for resolution.
            assert_eq!(run_libra_command(&["rev-parse", "HEAD"], p).stdout, head);
            assert_eq!(std::fs::read(p.join(".libra/index")).expect("index"), index);
            assert_eq!(std::fs::read(p.join("new")).expect("conflict"), content);
        }
    }
}

#[test]
fn merge_rename_conflict_no_commit_restart_preserves_conflict_stages() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for (label, build, conflicts) in rename_shape_table() {
            if !conflicts {
                continue;
            }
            // Given a no-commit merge paused at one of the rename conflicts.
            let repo = build();
            let p = repo.path();
            merge_expecting_conflict(p, &["merge", "--no-commit", "feature"], env);
            let stages = unmerged_stage_lines(p);
            // When restart rebuilds that conflict from the recorded target.
            merge_expecting_conflict(p, &["merge", "--restart"], env);
            // Then it produces the same stages and still finishes as a merge.
            assert_eq!(unmerged_stage_lines(p), stages, "{label}: restart stages");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "resolve");
            assert_cli_success(&run_libra_command(&["merge", "--continue"], p), "continue");
            let commit = run_libra_command(&["cat-file", "-p", "HEAD"], p);
            assert_cli_success(&commit, "inspect merge");
            assert_eq!(
                String::from_utf8_lossy(&commit.stdout)
                    .lines()
                    .filter(|line| line.starts_with("parent "))
                    .count(),
                2
            );
        }
    }
}

/// Git's suggest_conflicts saves the autostash for later application; applying
/// it while the squash index is unmerged must not replace conflict stages.
#[test]
fn merge_rename_conflict_squash_keeps_autostash_in_the_stash_list() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Given independent uncommitted work beside a rename/add collision.
        let repo = rename_add_repo();
        let p = repo.path();
        commit_file(p, "local.txt", "committed\n", "local baseline");
        std::fs::write(p.join("local.txt"), "uncommitted work\n").expect("local edit");
        // When squash pauses at its conflict, the autostash is made visible.
        merge_expecting_conflict(p, &["merge", "--squash", "--autostash", "feature"], env);
        // Then neither the conflict nor the saved local edit has disappeared.
        assert!(
            !unmerged_stage_lines(p).is_empty(),
            "squash still needs resolution"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("local.txt")).expect("local"),
            "committed\n"
        );
        assert!(
            !p.join(".libra/merge-autostash.json").exists(),
            "stash is available without a merge lifecycle"
        );
        let saved = run_libra_command(&["stash", "show", "-p", "stash@{0}"], p);
        assert_cli_success(&saved, "inspect saved autostash");
        assert!(String::from_utf8_lossy(&saved.stdout).contains("uncommitted work"));
    }
}

/// Git builtin/merge.c skips write_merge_state() for squash, including a
/// conflicted ort result: resolve and commit normally, without MERGE_HEAD.
#[test]
fn merge_rename_conflict_squash_finishes_as_an_ordinary_commit() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for (label, build, conflicts) in rename_shape_table() {
            if !conflicts {
                continue;
            }
            // Given an unresolved rename conflict and the original HEAD.
            let repo = build();
            let p = repo.path();
            let before = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&before, "original HEAD");
            // When the user requests a squash, it must not start a merge.
            let out = merge_expecting_conflict(p, &["merge", "--squash", "feature"], env);
            assert_eq!(
                run_libra_command(&["rev-parse", "HEAD"], p).stdout,
                before.stdout,
                "{label}: squash must not move HEAD"
            );
            assert!(
                !p.join(".libra/merge-state.json").exists(),
                "{label}: squash must not persist normal merge state"
            );
            let error = String::from_utf8_lossy(&out.stderr);
            assert!(error.contains("libra commit"), "{label}: {error}");
            assert!(!error.contains("merge --continue"), "{label}: {error}");
            for control in ["--continue", "--restart", "--abort"] {
                let out = run_libra_command(&["merge", control], p);
                assert!(!out.status.success(), "{label}: {control} needs a merge");
            }
            // Then staging the resolution and committing keeps one parent.
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "resolve");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "squashed", "--no-verify"], p),
                "commit squash",
            );
            let commit = run_libra_command(&["cat-file", "-p", "HEAD"], p);
            assert_cli_success(&commit, "inspect commit");
            let body = String::from_utf8_lossy(&commit.stdout);
            let parents: Vec<_> = body
                .lines()
                .filter(|line| line.starts_with("parent "))
                .collect();
            assert_eq!(
                parents,
                [format!(
                    "parent {}",
                    String::from_utf8_lossy(&before.stdout).trim()
                )]
            );
        }
    }
}
