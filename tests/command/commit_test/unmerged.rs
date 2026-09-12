//! An unresolved squash must never become a commit that drops conflict paths.

use super::*;

mod gitlinks;
mod path_boundaries;

fn squash_conflict_repo() -> tempfile::TempDir {
    squash_conflict_repo_at("conflict.txt")
}

fn squash_conflict_repo_at(conflict_path: &str) -> tempfile::TempDir {
    let repo = tempdir().expect("temporary repository");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::create_dir_all(p.join(conflict_path).parent().expect("conflict parent"))
        .expect("conflict directory");
    std::fs::write(p.join(conflict_path), "base\n").expect("base content");
    std::fs::write(p.join("keep.txt"), "keep\n").expect("unrelated content");
    assert_cli_success(
        &run_libra_command(&["add", conflict_path, "keep.txt"], p),
        "stage base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "base commit",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "feature branch",
    );
    for (branch, content) in [("main", "ours\n"), ("feature", "theirs\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        std::fs::write(p.join(conflict_path), content).expect("branch content");
        assert_cli_success(
            &run_libra_command(&["add", conflict_path], p),
            "stage branch",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", branch, "--no-verify"], p),
            "branch commit",
        );
    }
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    let merged = run_libra_command(&["merge", "--squash", "feature"], p);
    assert!(
        !merged.status.success(),
        "fixture must have a squash conflict"
    );
    assert!(
        !p.join(".libra/merge-state.json").exists(),
        "squash has no merge lifecycle"
    );
    let staged = run_libra_command(&["ls-files", "--stage"], p);
    assert_cli_success(&staged, "inspect conflict stages");
    for stage in [1, 2, 3] {
        assert!(
            String::from_utf8_lossy(&staged.stdout).contains(&format!(" {stage}\t{conflict_path}"))
        );
    }
    repo
}

#[test]
fn commit_rejects_unmerged_squash_without_mutating_repository() {
    for option in [None, Some("--allow-empty"), Some("--amend")] {
        // Given an unresolved squash with no MergeState and a stage-0 keep file.
        let repo = squash_conflict_repo();
        let p = repo.path();
        let head = run_libra_command(&["rev-parse", "HEAD"], p);
        assert_cli_success(&head, "original HEAD");
        let index = std::fs::read(p.join(".libra/index")).expect("original index");
        let conflict = std::fs::read(p.join("conflict.txt")).expect("conflict content");
        let mut args = vec!["commit", "-m", "unresolved", "--no-verify"];
        args.extend(option);

        // When a commit would otherwise omit unmerged paths.
        let output = run_libra_command(&args, p);

        // Then it fails with conflict guidance and preserves all user data.
        assert_eq!(output.status.code(), Some(128), "{args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("LBR-CONFLICT-001"), "{args:?}: {stderr}");
        assert!(stderr.contains("conflict.txt"), "{args:?}: {stderr}");
        assert!(stderr.contains("libra add"), "{args:?}: {stderr}");
        assert!(stderr.contains("libra commit -a"), "{args:?}: {stderr}");
        assert!(
            stderr.contains("libra update-index --cacheinfo"),
            "{args:?}: {stderr}"
        );
        assert_eq!(
            run_libra_command(&["rev-parse", "HEAD"], p).stdout,
            head.stdout,
            "{args:?}: HEAD"
        );
        assert_eq!(
            std::fs::read(p.join(".libra/index")).expect("index"),
            index,
            "{args:?}: index"
        );
        assert_eq!(
            std::fs::read(p.join("conflict.txt")).expect("conflict"),
            conflict,
            "{args:?}: conflict worktree"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("keep.txt")).expect("keep file"),
            "keep\n"
        );
    }
}

#[test]
fn commit_all_stages_unmerged_worktree_content_like_git() {
    for amend in [false, true] {
        // Given a squash conflict, Git -a treats even conflict markers as content.
        let repo = squash_conflict_repo();
        let p = repo.path();
        let content = std::fs::read(p.join("conflict.txt")).expect("conflict content");
        let mut args = vec!["commit", "-a", "-m", "auto-staged squash", "--no-verify"];
        if amend {
            args.push("--amend");
        }

        // When auto-stage explicitly selects all tracked worktree content.
        let output = run_libra_command(&args, p);

        // Then conflict stages become a committed stage-0 file, never a deletion.
        assert_cli_success(&output, "auto-stage squash conflict");
        let blob = run_libra_command(&["cat-file", "-p", "HEAD:conflict.txt"], p);
        assert_cli_success(&blob, "auto-staged conflict content");
        assert_eq!(blob.stdout, content);
        let staged = run_libra_command(&["ls-files", "--stage"], p);
        assert_cli_success(&staged, "resolved index");
        let stages = String::from_utf8_lossy(&staged.stdout);
        assert!(stages.contains(" 0\tconflict.txt"));
        for stage in [1, 2, 3] {
            assert!(!stages.contains(&format!(" {stage}\tconflict.txt")));
        }
    }
}

#[test]
fn commit_all_stages_deleted_conflict_as_a_resolution() {
    // Given the user removed a conflicted regular file from the worktree.
    let repo = squash_conflict_repo();
    let p = repo.path();
    std::fs::remove_file(p.join("conflict.txt")).expect("delete conflict");

    // When -a commits that explicit deletion resolution.
    let output = run_libra_command(&["commit", "-a", "-m", "remove conflict", "--no-verify"], p);

    // Then no conflict stage survives and the committed tree records the deletion.
    assert_cli_success(&output, "commit deletion resolution");
    let staged = run_libra_command(&["ls-files", "--stage"], p);
    assert_cli_success(&staged, "resolved index");
    assert!(!String::from_utf8_lossy(&staged.stdout).contains("conflict.txt"));
    let tree = run_libra_command(&["ls-tree", "HEAD"], p);
    assert_cli_success(&tree, "resolved tree");
    assert!(!String::from_utf8_lossy(&tree.stdout).contains("conflict.txt"));
    assert!(String::from_utf8_lossy(&tree.stdout).contains("keep.txt"));
}

#[cfg(unix)]
#[test]
fn commit_all_stages_symlink_conflict_without_following_it() {
    // Given a conflict resolved as a symlink to an existing tracked file.
    let repo = squash_conflict_repo();
    let p = repo.path();
    std::fs::remove_file(p.join("conflict.txt")).expect("remove conflict file");
    std::os::unix::fs::symlink("keep.txt", p.join("conflict.txt")).expect("symlink resolution");

    // When -a stages the worktree resolution.
    let output = run_libra_command(
        &["commit", "-a", "-m", "symlink resolution", "--no-verify"],
        p,
    );

    // Then the committed link contains its target name, never the target content.
    assert_cli_success(&output, "commit symlink resolution");
    let blob = run_libra_command(&["cat-file", "-p", "HEAD:conflict.txt"], p);
    assert_cli_success(&blob, "symlink blob");
    assert_eq!(blob.stdout, b"keep.txt");
    let tree = run_libra_command(&["ls-tree", "HEAD"], p);
    assert_cli_success(&tree, "symlink tree");
    assert!(
        String::from_utf8_lossy(&tree.stdout)
            .lines()
            .any(|line| line.starts_with("120000 blob ") && line.ends_with("\tconflict.txt"))
    );
}

#[test]
fn commit_preview_preserves_unmerged_squash_state() {
    for option in ["--dry-run", "--porcelain"] {
        for all in [false, true] {
            // Given a squash with unresolved index stages and conflict content.
            let repo = squash_conflict_repo();
            let p = repo.path();
            let head = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&head, "original HEAD");
            let index = std::fs::read(p.join(".libra/index")).expect("original index");
            let content = std::fs::read(p.join("conflict.txt")).expect("original content");
            let mut args = vec!["commit", option, "-m", "preview"];
            if all {
                args.push("-a");
            }

            // When previewing with or without the temporary auto-staged index.
            let output = run_libra_command(&args, p);

            // Then no commit, index rewrite, or content mutation is persisted.
            assert_cli_success(&output, "completed conflict preview");
            assert!(!String::from_utf8_lossy(&output.stderr).contains("LBR-CONFLICT-001"));
            assert_eq!(
                run_libra_command(&["rev-parse", "HEAD"], p).stdout,
                head.stdout
            );
            assert_eq!(std::fs::read(p.join(".libra/index")).expect("index"), index);
            assert_eq!(
                std::fs::read(p.join("conflict.txt")).expect("content"),
                content
            );
        }
    }
}

#[test]
fn commit_unmerged_error_display_and_stable_code_are_pinned() {
    // Given an unresolved path reported by the commit index preflight.
    let error =
        libra::command::commit::CommitError::UnresolvedConflicts("conflict.txt".to_string());
    // When the domain error is formatted and converted for the CLI.
    let message = error.to_string();
    let cli = libra::utils::error::CliError::from(error);
    // Then scripts receive the existing conflict code and users see the path.
    assert_eq!(
        message,
        "cannot commit with unresolved conflicts: conflict.txt"
    );
    assert_eq!(cli.stable_code().as_str(), "LBR-CONFLICT-001");
    assert_eq!(cli.exit_code(), 128);
}

#[test]
fn commit_accepts_explicitly_staged_squash_resolution() {
    // Given a squash whose content conflict was resolved and explicitly staged.
    let repo = squash_conflict_repo();
    let p = repo.path();
    std::fs::write(p.join("conflict.txt"), "resolved\n").expect("resolved content");
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], p),
        "stage resolution",
    );

    // When the user commits the resolution normally.
    let output = run_libra_command(&["commit", "-m", "resolved squash", "--no-verify"], p);

    // Then the conflict path is present in a successful single-parent commit.
    assert_cli_success(&output, "commit resolved squash");
    let blob = run_libra_command(&["cat-file", "-p", "HEAD:conflict.txt"], p);
    assert_cli_success(&blob, "committed resolution");
    assert_eq!(blob.stdout, b"resolved\n");
    let commit = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&commit, "inspect commit");
    assert_eq!(
        String::from_utf8_lossy(&commit.stdout)
            .lines()
            .filter(|line| line.starts_with("parent "))
            .count(),
        1
    );
}
