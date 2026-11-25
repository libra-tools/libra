//! Regression tests for removed `init` separate-directory flags.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use tempfile::tempdir;

use super::{assert_cli_success, init_repo_via_cli, run_libra_command};

#[test]
fn init_rejects_separate_libra_dir_flag() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let storage = temp_root.path().join("storage");

    let output = run_libra_command(
        &["init", "--separate-libra-dir", storage.to_str().unwrap()],
        &repo,
    );
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not implemented"),
        "expected intentional-difference error, got: {stderr}"
    );
    assert!(
        stderr.contains("from-git-repository"),
        "expected migration hint, got: {stderr}"
    );
    assert!(
        !repo.join(".libra").exists(),
        "init should not create .libra when parse fails"
    );
}

#[test]
fn init_rejects_separate_git_dir_alias() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let storage = temp_root.path().join("storage");

    let output = run_libra_command(
        &["init", "--separate-git-dir", storage.to_str().unwrap()],
        &repo,
    );
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not implemented"),
        "expected intentional-difference error, got: {stderr}"
    );
    assert!(
        stderr.contains("from-git-repository"),
        "expected migration hint, got: {stderr}"
    );
    assert!(
        !repo.join(".libra").exists(),
        "init should not create .libra when parse fails"
    );
}

#[test]
fn legacy_separate_layout_repo_is_no_longer_detected() {
    let temp_root = tempdir().unwrap();
    let storage_holder = temp_root.path().join("storage-holder");
    init_repo_via_cli(&storage_holder);

    let workdir = temp_root.path().join("legacy-worktree");
    fs::create_dir_all(&workdir).unwrap();

    let storage_dir = storage_holder.join(".libra").canonicalize().unwrap();
    fs::write(
        workdir.join(".libra"),
        format!("gitdir: {}\n", storage_dir.display()),
    )
    .unwrap();

    let status = run_libra_command(&["status"], &workdir);
    assert_ne!(status.status.code(), Some(0));
    let status_stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        status_stderr.contains("not a libra repository"),
        "legacy layout should no longer be recognized: {status_stderr}"
    );

    let config = run_libra_command(&["config", "list"], &workdir);
    assert_ne!(config.status.code(), Some(0));
    let config_stderr = String::from_utf8_lossy(&config.stderr);
    assert!(
        config_stderr.contains("not a libra repository"),
        "legacy layout should fail for config commands as well: {config_stderr}"
    );

    let sanity = run_libra_command(&["status"], &storage_holder);
    assert_cli_success(&sanity, "storage-holder repo should remain healthy");
}

#[test]
fn test_init_bare_and_separate_git_dir_rejected() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let storage = temp_root.path().join("storage");

    let output = run_libra_command(
        &[
            "init",
            "--bare",
            "--separate-git-dir",
            storage.to_str().unwrap(),
        ],
        &repo,
    );
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented"), "{stderr}");
    assert!(stderr.contains("from-git-repository"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn test_init_ref_format_files_rejected() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init", "--ref-format=files"], &repo);
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ref-format"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn test_init_ref_format_reftable_rejected() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init", "--ref-format=reftable"], &repo);
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ref-format"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn test_init_recurse_submodules_rejected() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init", "--recurse-submodules"], &repo);
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("submodule"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn test_init_unknown_flag_still_unexpected_argument() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init", "--not-a-real-init-flag"], &repo);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");
}
