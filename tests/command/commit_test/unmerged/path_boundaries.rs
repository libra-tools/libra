//! Auto-stage must not read through replaced parents or discard nested repos.

use super::*;

#[test]
fn commit_all_resolves_deleted_conflict_below_a_replaced_directory() {
    // Given the user replaces a conflicted file's parent with an untracked file.
    let repo = squash_conflict_repo_at("directory/conflict.txt");
    let p = repo.path();
    std::fs::remove_file(p.join("directory/conflict.txt")).expect("remove conflict");
    std::fs::remove_dir(p.join("directory")).expect("remove directory");
    std::fs::write(p.join("directory"), "replacement\n").expect("replacement file");

    // When -a stages tracked deletions without adding the untracked replacement.
    let output = run_libra_command(
        &[
            "commit",
            "-a",
            "-m",
            "delete nested conflict",
            "--no-verify",
        ],
        p,
    );

    // Then the old conflict stages disappear and replacement content is untouched.
    assert_cli_success(&output, "commit nested deletion");
    let index = run_libra_command(&["ls-files", "--stage"], p);
    assert_cli_success(&index, "resolved index");
    assert!(!String::from_utf8_lossy(&index.stdout).contains("directory"));
    let tree = run_libra_command(&["ls-tree", "HEAD"], p);
    assert_cli_success(&tree, "resolved tree");
    assert!(!String::from_utf8_lossy(&tree.stdout).contains("directory"));
    assert_eq!(
        std::fs::read_to_string(p.join("directory")).expect("replacement"),
        "replacement\n"
    );
}

#[cfg(unix)]
#[test]
fn commit_all_never_reads_conflicts_through_symlinked_parents() {
    // Given the conflict's parent was replaced with a symlink outside the repo.
    let repo = squash_conflict_repo_at("directory/conflict.txt");
    let p = repo.path();
    let external = tempdir().expect("external directory");
    let secret = b"outside-only sentinel\n";
    std::fs::write(external.path().join("conflict.txt"), secret).expect("external sentinel");
    let hash_output = run_libra_command_with_stdin(
        &["hash-object", "--stdin"],
        p,
        std::str::from_utf8(secret).expect("sentinel"),
    );
    assert_cli_success(&hash_output, "sentinel hash without storing it");
    let hash = String::from_utf8_lossy(&hash_output.stdout);
    let object = loose_object_path(p, hash.trim());
    assert!(!object.exists(), "sentinel is not already stored");
    std::fs::remove_file(p.join("directory/conflict.txt")).expect("remove conflict");
    std::fs::remove_dir(p.join("directory")).expect("remove directory");
    std::os::unix::fs::symlink(external.path(), p.join("directory")).expect("external symlink");

    // When -a handles the tracked child as removed, matching Git's leading-path check.
    let output = run_libra_command(
        &[
            "commit",
            "-a",
            "-m",
            "remove linked conflict",
            "--no-verify",
        ],
        p,
    );

    // Then no external content enters the index, tree, or object store.
    assert_cli_success(&output, "commit deletion through linked parent");
    let index = run_libra_command(&["ls-files", "--stage"], p);
    assert_cli_success(&index, "resolved index");
    assert!(!String::from_utf8_lossy(&index.stdout).contains("directory"));
    assert!(
        !object.exists(),
        "outside sentinel must not be stored as an object"
    );
    assert_eq!(
        std::fs::read(external.path().join("conflict.txt")).expect("sentinel"),
        secret
    );
    assert_eq!(
        std::fs::read_link(p.join("directory")).expect("symlink"),
        external.path()
    );
}

#[test]
fn commit_all_rejects_nested_repository_as_an_implicit_conflict_deletion() {
    for marker in [".git", ".libra"] {
        // Given a conflicted file is replaced by a directory containing repo metadata.
        let repo = squash_conflict_repo();
        let p = repo.path();
        std::fs::remove_file(p.join("conflict.txt")).expect("remove conflict");
        std::fs::create_dir_all(p.join("conflict.txt").join(marker))
            .expect("nested repository marker");
        std::fs::write(p.join("keep.txt"), "unrelated work\n").expect("local change");
        let index = std::fs::read(p.join(".libra/index")).expect("original index");
        let head = run_libra_command(&["rev-parse", "HEAD"], p);
        assert_cli_success(&head, "original HEAD");

        // When -a cannot determine an explicit submodule resolution.
        let output = run_libra_command(&["commit", "-a", "-m", "nested repo", "--no-verify"], p);

        // Then it refuses without deleting conflict stages or staging unrelated work.
        assert_eq!(output.status.code(), Some(128));
        assert!(String::from_utf8_lossy(&output.stderr).contains("LBR-CONFLICT-001"));
        assert_eq!(std::fs::read(p.join(".libra/index")).expect("index"), index);
        assert_eq!(
            run_libra_command(&["rev-parse", "HEAD"], p).stdout,
            head.stdout
        );
        assert!(p.join("conflict.txt").join(marker).is_dir());
        assert_eq!(
            std::fs::read_to_string(p.join("keep.txt")).expect("local change"),
            "unrelated work\n"
        );
    }
}
