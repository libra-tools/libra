//! `tests/compat/compat_shared_repo_permissions.rs` — post-sequence permission sweep guard (Issue #473 IN-07).
//!
//! Pins the invariant that in a shared repository, after a fixed sequence of commands
//! (`init`, `config`, `add`, `commit`, `status`, `stash`, `repack`, `maintenance`):
//! - Every directory in `.libra` is group-traversable (`mode & 0010 != 0`) and carries setgid (`mode & 02000 != 0`).
//! - Every file (except allowlisted owner-only files like `vault.db*`) is group-readable (`mode & 0040 != 0`).
//! - No file or directory in `.libra` is world-writable (`mode & 0002 == 0`).
//! - Owner-only allowlisted files (`vault.db*`) remain strictly owner-only.

use std::{fs, path::Path, process::Command};

use tempfile::tempdir;

fn libra_bin() -> &'static str {
    env!("CARGO_BIN_EXE_libra")
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    Command::new(libra_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to spawn libra binary")
}

#[cfg(unix)]
#[test]
fn shared_repo_permissions_sweep_guard() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");

    let out = run(&["init", "--shared=group"], &repo);
    assert!(out.status.success(), "init --shared=group");
    assert!(
        run(&["config", "user.name", "Test User"], &repo)
            .status
            .success()
    );
    assert!(
        run(&["config", "user.email", "test@example.com"], &repo)
            .status
            .success()
    );

    // Sequence of operations
    fs::write(repo.join("file1.txt"), "first file\n").expect("write file1");
    assert!(run(&["add", "file1.txt"], &repo).status.success());
    assert!(run(&["commit", "-m", "commit 1"], &repo).status.success());

    assert!(run(&["status"], &repo).status.success());

    fs::write(repo.join("file1.txt"), "modified file\n").expect("modify file1");
    assert!(
        run(&["stash", "push", "-m", "wip stash"], &repo)
            .status
            .success()
    );
    assert!(run(&["stash", "pop"], &repo).status.success());

    assert!(run(&["repack", "-a"], &repo).status.success());
    assert!(run(&["maintenance", "run"], &repo).status.success());

    let dot_libra = repo.join(".libra");
    assert!(dot_libra.is_dir());

    for entry in walkdir::WalkDir::new(&dot_libra) {
        let entry = entry.expect("walkdir entry");
        let path = entry.path();
        let metadata = fs::symlink_metadata(path).expect("symlink_metadata");
        if metadata.file_type().is_symlink() {
            continue;
        }

        let mode = metadata.permissions().mode() & 0o7777;

        // Invariant 1: No file or directory in .libra is world-writable
        assert_eq!(
            mode & 0o002,
            0,
            "path {} must not be world-writable: mode {mode:o}",
            path.display()
        );

        if metadata.is_dir() {
            // Invariant 2: Directories in a group-shared repo are group-traversable
            assert_ne!(
                mode & 0o010,
                0,
                "directory {} must be group-traversable: mode {mode:o}",
                path.display()
            );
            // Invariant 3: Directories in a group-shared repo carry setgid
            assert_ne!(
                mode & 0o2000,
                0,
                "directory {} must carry setgid bit: mode {mode:o}",
                path.display()
            );
        } else if libra::utils::shared_perm::is_owner_only_path(path) {
            // Invariant 4: Owner-only allowlisted files remain owner-only
            assert_eq!(
                mode & 0o077,
                0,
                "owner-only file {} must have zero group/other permissions: mode {mode:o}",
                path.display()
            );
        } else {
            // Invariant 5: Non-allowlisted files in group-shared repo are group-readable
            assert_ne!(
                mode & 0o040,
                0,
                "file {} must be group-readable: mode {mode:o}",
                path.display()
            );
        }
    }
}
