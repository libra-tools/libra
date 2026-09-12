//! MG-06: binary rename merges preserve Git's content and mode selection.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{
    assert_cli_success, create_committed_repo_via_cli, index_stage_lines, merge_expecting_conflict,
    run_libra_command, stage_blob, unmerged_stage_lines,
};

fn content(label: &str) -> Vec<u8> {
    format!(
        "binary\0header\n{}{label}\n",
        "unchanged record\n".repeat(128)
    )
    .into_bytes()
}

fn commit_changes(p: &Path, message: &str) {
    assert_cli_success(
        &run_libra_command(&["add", "-A", "."], p),
        "stage binary changes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
        "commit binary changes",
    );
}

fn crisscross(executable: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("old"), content("base")).expect("original binary");
    commit_changes(p, "binary root");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    for (branch, destination) in [("main", "a"), ("feature", "b")] {
        assert_cli_success(&run_libra_command(&["checkout", branch], p), "base branch");
        std::fs::rename(p.join("old"), p.join(destination)).expect("rename original");
        std::fs::write(p.join(destination), content(branch)).expect("divergent binary edit");
        if executable && branch == "main" {
            #[cfg(unix)]
            std::fs::set_permissions(p.join(destination), std::fs::Permissions::from_mode(0o755))
                .expect("executable rename");
        }
        commit_changes(p, branch);
    }
    for (from, other, tip) in [("main", "feature", "x"), ("feature", "main", "y")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout base");
        assert_cli_success(
            &run_libra_command(&["checkout", "-b", tip], p),
            "create arm",
        );
        merge_expecting_conflict(p, &["merge", other], &[]);
        for path in ["a", "b"] {
            std::fs::write(p.join(path), content(tip)).expect("resolve binary arm");
        }
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage arm");
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "commit two-parent arm",
        );
    }
    let bases = run_libra_command(&["merge-base", "--all", "x", "y"], p);
    assert_cli_success(&bases, "enumerate bases");
    assert_eq!(String::from_utf8_lossy(&bases.stdout).lines().count(), 2);
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout ours");
    repo
}

fn assert_original_fold(executable: bool) {
    // Given: both bases rename and independently edit a binary source.
    let repo = crisscross(executable);
    let p = repo.path();
    let mut walks = Vec::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // When: the outer merge consumes their recursively merged ancestor.
        merge_expecting_conflict(p, &["merge", "y"], env);
        // Then: Git ll_binary_merge(virtual_ancestor) selects the ROOT bytes,
        // while handle_content_merge preserves its independently merged mode.
        let mut worktree = Vec::new();
        for path in ["a", "b"] {
            let stages = index_stage_lines(p, path);
            assert_eq!(stages.len(), 3, "all three binary stages at {path}");
            for (stage, expected) in [(1, "base"), (2, "x"), (3, "y")] {
                let marker = format!(" {stage}\t");
                let line = stages
                    .iter()
                    .find(|line| line.contains(&marker))
                    .expect("stage entry");
                let blob = run_libra_command(&["cat-file", "-p", &stage_blob(line)], p);
                assert_cli_success(&blob, "read staged binary");
                assert_eq!(
                    blob.stdout,
                    content(expected),
                    "{path}@{stage} must match Git"
                );
                if stage == 1 {
                    let mode = if executable { "100755 " } else { "100644 " };
                    assert!(line.starts_with(mode), "ancestor mode at {path}: {line}");
                }
            }
            worktree.push(std::fs::read(p.join(path)).expect("binary worktree"));
        }
        assert!(index_stage_lines(p, "old").is_empty());
        assert!(!p.join("old").exists());
        let stages = unmerged_stage_lines(p);
        assert_eq!(stages.len(), 6);
        walks.push((stages, worktree));
        assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "restore tips");
    }
    assert_eq!(
        walks[0], walks[1],
        "both walks consume the same binary ancestor"
    );
}

#[test]
fn merge_fold_binary_1to2_uses_root_bytes_at_both_destinations() {
    assert_original_fold(false);
}

#[test]
#[cfg(unix)]
fn merge_fold_binary_1to2_preserves_independently_merged_executable_mode() {
    assert_original_fold(true);
}

#[test]
#[cfg(unix)]
fn merge_binary_1to2_keeps_the_merged_mode_when_selecting_content() {
    for executable_branch in ["main", "feature"] {
        // Given: both binary contents diverged, and exactly one side chmods.
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("old"), content("base")).expect("base binary");
        commit_changes(p, "root");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        for (branch, path) in [("main", "a"), ("feature", "b")] {
            assert_cli_success(&run_libra_command(&["checkout", branch], p), "side");
            std::fs::rename(p.join("old"), p.join(path)).expect("rename source");
            std::fs::write(p.join(path), content(branch)).expect("binary edit");
            let mode = if branch == executable_branch {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(p.join(path), std::fs::Permissions::from_mode(mode))
                .expect("side mode");
            commit_changes(p, branch);
        }
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "ours");
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            for (args, chosen, unmergeable) in [
                (&["merge", "feature"][..], "main", true),
                (&["merge", "-X", "ours", "feature"][..], "main", false),
                (&["merge", "-X", "theirs", "feature"][..], "feature", false),
            ] {
                // When: binary selection settles content but leaves the 1to2 path conflict.
                merge_expecting_conflict(p, args, env);
                // Then: Git's mode+OID was_binary test preserves theirs only when
                // the unclean result exactly equals the original ours entry.
                for (path, stage) in [("a", 2), ("b", 3)] {
                    let preserve_theirs = unmergeable && executable_branch == "main" && path == "b";
                    let (mode, chosen) = if preserve_theirs {
                        ("100644", "feature")
                    } else {
                        ("100755", chosen)
                    };
                    let stages = index_stage_lines(p, path);
                    assert_eq!(stages.len(), 1, "one side-stage at {path}");
                    assert!(
                        stages[0].starts_with(mode) && stages[0].contains(&format!(" {stage}\t")),
                        "{}",
                        stages[0]
                    );
                    let blob = run_libra_command(&["cat-file", "-p", &stage_blob(&stages[0])], p);
                    assert_cli_success(&blob, "read selected binary");
                    assert_eq!(blob.stdout, content(chosen), "{path}: Git's selected bytes");
                    assert_eq!(
                        std::fs::read(p.join(path)).expect("worktree binary"),
                        content(chosen)
                    );
                    assert_eq!(
                        std::fs::metadata(p.join(path))
                            .expect("worktree mode")
                            .permissions()
                            .mode()
                            & 0o111
                            != 0,
                        mode == "100755"
                    );
                }
                assert_cli_success(
                    &run_libra_command(&["merge", "--abort"], p),
                    "restore sides",
                );
            }
        }
    }
}
