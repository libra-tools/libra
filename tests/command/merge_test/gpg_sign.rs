//! End-to-end coverage for vault-signed merge commits.

use std::path::Path;

use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, commit_file, configure_identity_via_cli, head_commit, run_libra_command,
    run_libra_command_with_stdin,
};

fn divergent_merge_repo(with_vault_key: bool) -> TempDir {
    let repo = tempdir().expect("create merge signing repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize merge signing repository",
    );
    configure_identity_via_cli(path);
    if with_vault_key {
        assert_cli_success(
            &run_libra_command(&["config", "generate-gpg-key"], path),
            "generate repository vault signing key",
        );
    }
    commit_file(path, "base.txt", "base\n", "base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature branch",
    );
    commit_file(path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main branch",
    );
    commit_file(path, "main.txt", "main\n", "main change");
    repo
}

fn raw_head_commit(repo: &Path) -> String {
    let output = run_libra_command_with_stdin(&["cat-file", "--batch"], repo, "HEAD\n");
    assert_cli_success(&output, "read raw HEAD commit");
    String::from_utf8(output.stdout).expect("commit object must be utf-8")
}

fn assert_head_is_signed(repo: &Path, context: &str) {
    let raw = raw_head_commit(repo);
    assert!(
        raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "{context} must create a vault-signed merge commit: {raw}"
    );
}

#[test]
fn merge_gpg_sign_automatic_merge_is_signed() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "feature"], repo.path()),
        "merge -S feature",
    );
    assert_head_is_signed(repo.path(), "automatic merge");
}

#[test]
fn merge_gpg_sign_continue_keeps_explicit_signing() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "--no-commit", "feature"], repo.path()),
        "start signed no-commit merge",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], repo.path()),
        "continue signed merge",
    );
    assert_head_is_signed(repo.path(), "continued merge");
}

#[test]
fn merge_gpg_sign_ours_strategy_is_signed() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "-s", "ours", "feature"], repo.path()),
        "merge -S -s ours feature",
    );
    assert_head_is_signed(repo.path(), "ours merge");
}

#[test]
fn merge_gpg_sign_octopus_merge_is_signed() {
    let repo = divergent_merge_repo(true);
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "second"], path),
        "create second octopus branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "second"], path),
        "checkout second octopus branch",
    );
    commit_file(path, "second.txt", "second\n", "second change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to octopus main branch",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "feature", "second"], path),
        "signed octopus merge",
    );
    assert_head_is_signed(path, "octopus merge");
}

#[test]
fn merge_gpg_sign_no_gpg_sign_overrides_every_config_default() {
    let repo = divergent_merge_repo(true);
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], path),
        "force signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--no-gpg-sign", "feature"], path),
        "merge with signing explicitly disabled",
    );
    let raw = raw_head_commit(path);
    assert!(
        !raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "--no-gpg-sign must override commit.gpgSign: {raw}"
    );
}

#[test]
fn merge_gpg_sign_config_priority_matches_commit() {
    let force = divergent_merge_repo(true);
    let force_path = force.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], force_path),
        "disable vault default",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], force_path),
        "force signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], force_path),
        "merge with commit.gpgSign=true",
    );
    assert_head_is_signed(force_path, "commit.gpgSign=true merge");

    let fallback = divergent_merge_repo(true);
    let fallback_path = fallback.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], fallback_path),
        "enable the vault signing fallback",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], fallback_path),
        "merge with the vault signing fallback",
    );
    assert_head_is_signed(fallback_path, "vault.signing fallback merge");

    let disable = divergent_merge_repo(true);
    let disable_path = disable.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], disable_path),
        "enable vault default",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "false"], disable_path),
        "disable signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], disable_path),
        "merge with commit.gpgSign=false",
    );
    let raw = raw_head_commit(disable_path);
    assert!(
        !raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "commit.gpgSign=false must override vault.signing: {raw}"
    );
}

#[test]
fn merge_gpg_sign_failure_keeps_head_unchanged() {
    let repo = divergent_merge_repo(false);
    let path = repo.path();
    let before = head_commit(path);
    let output = run_libra_command(&["merge", "-S", "feature"], path);
    assert!(
        !output.status.success(),
        "-S must fail when the vault has no unseal key"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unseal key"),
        "the signing failure must explain how the vault is unavailable: {stderr}"
    );
    assert_eq!(
        head_commit(path),
        before,
        "a signing failure must not create or move to a merge commit"
    );
}
