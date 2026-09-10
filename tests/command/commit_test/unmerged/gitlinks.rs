//! Unmaterialized submodules are not implicit deletion resolutions for `-a`.

use super::*;

fn gitlink_conflict_repo() -> tempfile::TempDir {
    let repo = squash_conflict_repo();
    let p = repo.path();
    let index_path = p.join(".libra/index");
    let mut index = git_internal::internal::index::Index::load(&index_path).expect("index");
    for (stage, revision) in [(1, "HEAD~1"), (2, "HEAD"), (3, "feature")] {
        let output = run_libra_command(&["rev-parse", revision], p);
        assert_cli_success(&output, "gitlink commit");
        let hash = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("object id");
        let mut entry = index.remove("conflict.txt", stage).expect("conflict stage");
        entry.hash = hash;
        entry.mode = 0o160000;
        index.update(entry);
    }
    index.save(&index_path).expect("gitlink conflict index");
    std::fs::remove_file(p.join("conflict.txt")).expect("unmaterialized submodule");
    repo
}

#[test]
fn commit_all_preserves_unresolved_gitlink_and_unrelated_work() {
    for materialized in [false, true] {
        // Given an unresolved submodule plus unrelated tracked worktree changes.
        let repo = gitlink_conflict_repo();
        let p = repo.path();
        if materialized {
            std::fs::create_dir(p.join("conflict.txt")).expect("submodule directory");
            std::fs::write(p.join("conflict.txt/local.txt"), "submodule work\n")
                .expect("submodule file");
        }
        std::fs::write(p.join("keep.txt"), "unrelated work\n").expect("local change");
        let head = run_libra_command(&["rev-parse", "HEAD"], p);
        assert_cli_success(&head, "original HEAD");
        let index = std::fs::read(p.join(".libra/index")).expect("original index");

        // When -a has no explicit stage-0 submodule commit to select.
        let output = run_libra_command(
            &["commit", "-a", "-m", "unresolved gitlink", "--no-verify"],
            p,
        );

        // Then it refuses before staging unrelated work or deleting the gitlink.
        assert_eq!(output.status.code(), Some(128));
        assert!(String::from_utf8_lossy(&output.stderr).contains("LBR-CONFLICT-001"));
        assert_eq!(
            run_libra_command(&["rev-parse", "HEAD"], p).stdout,
            head.stdout
        );
        assert_eq!(std::fs::read(p.join(".libra/index")).expect("index"), index);
        assert_eq!(
            std::fs::read_to_string(p.join("keep.txt")).expect("local file"),
            "unrelated work\n"
        );
        if materialized {
            assert_eq!(
                std::fs::read_to_string(p.join("conflict.txt/local.txt")).expect("submodule file"),
                "submodule work\n"
            );
        } else {
            assert!(!p.join("conflict.txt").exists());
        }
    }
}

#[test]
fn commit_accepts_explicit_stage_zero_gitlink_resolution() {
    // Given a submodule conflict resolved by selecting one commit explicitly.
    let repo = gitlink_conflict_repo();
    let p = repo.path();
    let target = run_libra_command(&["rev-parse", "feature"], p);
    assert_cli_success(&target, "resolved gitlink commit");
    let hash = String::from_utf8_lossy(&target.stdout);
    let cacheinfo = format!("160000,{},conflict.txt", hash.trim());
    assert_cli_success(
        &run_libra_command(&["update-index", "--cacheinfo", &cacheinfo], p),
        "stage gitlink resolution",
    );

    // When the resolved index is committed without materializing the submodule.
    let output = run_libra_command(&["commit", "-m", "resolved gitlink", "--no-verify"], p);

    // Then the selected gitlink is in the tree rather than disappearing.
    assert_cli_success(&output, "commit gitlink resolution");
    let tree = run_libra_command(&["ls-tree", "HEAD"], p);
    assert_cli_success(&tree, "committed tree");
    assert!(
        String::from_utf8_lossy(&tree.stdout)
            .contains(&format!("160000 commit {}\tconflict.txt", hash.trim()))
    );
    assert!(!p.join("conflict.txt").exists());
}
